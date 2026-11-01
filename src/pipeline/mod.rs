//! The pipeline: one capture attempt to one postprocessed frame, and its replacement as a unit.
//!
//! The pipeline owns the capture, the shared frame, the preprocess, and the inference session.
//! The engine sees no fence, no texture, and no fence value. This module has no `unsafe`.

mod model;

use std::{path::Path, sync::Arc};

use anyhow::Result;

use self::model::ModelHalf;
use crate::{
    graphics::{
        CapturePoll, DisplayInfo, DxgiCapture, GpuFaultReport, NoDesktopUpdateReason,
        gpu_diagnostics::{FaultSite, FenceProgress, GpuFaultStage, collect_gpu_fault_report},
        profile_sampler::ProfileSampler,
        shared_frame::CaptureFrame,
    },
    inference::{Candidates, Detection, EpAssignmentState, TensorPrecision},
    settings::ConfidenceThreshold,
    telemetry::FrameTimes,
    timing::QpcClock,
};

/// What the engine asks of a pipeline when it is built or its model is replaced.
pub(crate) struct PipelineConfig<'a> {
    pub model: &'a Path,
    pub pipeline_id: u64,
    pub confidence_threshold: ConfidenceThreshold,
    pub low_latency: bool,
    pub qpc: QpcClock,
}

/// A new frame after the inference and the postprocess.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcessedFrame {
    pub frame_seq: u64,
    pub accumulated_frames: u32,
    pub desktop_size: (usize, usize),
    pub times: FrameTimes,
    pub gpu_preprocess_ms: Option<f64>,
    pub gpu_copy_ms: Option<f64>,
    pub selected: Option<Detection>,
    pub candidates: Candidates,
}

// The frame is handed over by value, so the frame path does not allocate.
#[allow(clippy::large_enum_variant)]
pub(crate) enum FrameOutcome {
    NewFrame(ProcessedFrame),
    NoDesktopUpdate(NoDesktopUpdateReason),
}

pub(crate) enum FaultKind {
    /// The GPU transport is poisoned. The engine rebuilds the pipeline one time.
    Poisoned { device_lost: bool },
    /// The pipeline cannot be used. The engine suspends it.
    Invalid,
}

pub(crate) struct PipelineFault {
    pub kind: FaultKind,
    pub message: String,
    /// `generation` is empty. The engine fills it. Boxed, so that a fault result stays small.
    pub report: Option<Box<GpuFaultReport>>,
}

pub(crate) struct Pipeline {
    // Drop order: the model half, then the D3D11 half of the shared frame, then the capture last.
    model: ModelHalf,
    capture_frame: CaptureFrame,
    capture: DxgiCapture,
    qpc: QpcClock,
    id: u64,
    /// The New frames of this pipeline. Each frame uses its count as the fence value.
    frame_seq: u64,
    /// The fence value of the last submitted frame. The shared frame is free once it completes.
    last_submitted: u64,
    sampler: ProfileSampler,
}

impl Pipeline {
    /// Builds a pipeline on `capture`. On failure the capture comes back with the error.
    // The capture comes back by value on failure. A build runs once per model or display change.
    #[allow(clippy::result_large_err)]
    pub(crate) fn build(
        mut capture: DxgiCapture,
        config: &PipelineConfig<'_>,
    ) -> Result<Self, (DxgiCapture, anyhow::Error)> {
        let (model, capture_frame) = match model::build(&capture, config) {
            Ok(built) => built,
            Err(error) => return Err((capture, error)),
        };
        if let Err(error) = capture.bind_copy_target(&capture_frame, model.input_size) {
            return Err((capture, error));
        }
        Ok(Self {
            model,
            capture_frame,
            capture,
            qpc: config.qpc,
            id: config.pipeline_id,
            frame_seq: 0,
            last_submitted: 0,
            sampler: ProfileSampler::default(),
        })
    }

    /// Builds the model half again on the same capture. On failure the pipeline is not changed.
    pub(crate) fn replace_model(&mut self, config: &PipelineConfig<'_>) -> Result<()> {
        let (model, capture_frame) = model::build(&self.capture, config)?;
        self.capture
            .bind_copy_target(&capture_frame, model.input_size)?;
        // Dropping the old model waits for its GPU queue.
        let old_model = std::mem::replace(&mut self.model, model);
        let old_frame = std::mem::replace(&mut self.capture_frame, capture_frame);
        drop(old_model);
        drop(old_frame);
        self.qpc = config.qpc;
        self.id = config.pipeline_id;
        // The new shared frame has new fences, so the frame count starts again.
        self.frame_seq = 0;
        self.last_submitted = 0;
        Ok(())
    }

    /// One capture attempt and, for a new frame, the copy, the preprocess, the inference, and the postprocess.
    pub(crate) fn process_next(&mut self, wait_ms: u32) -> Result<FrameOutcome, PipelineFault> {
        let reuse_completed = self.check_reuse_fence()?;

        let attempt_start = QpcClock::now();
        let frame = match self.capture.acquire(wait_ms) {
            Ok(CapturePoll::Fresh(frame)) => frame,
            Ok(CapturePoll::NoDesktopUpdate(reason)) => {
                return Ok(FrameOutcome::NoDesktopUpdate(reason));
            }
            Err(error) => return Err(self.capture_fault(&error)),
        };
        let acquired_qpc = QpcClock::now();
        let frame_seq = self
            .frame_seq
            .checked_add(1)
            .expect("frame sequence exhausted");
        self.frame_seq = frame_seq;

        let measure = self.sampler.should_measure();
        let copy = match self.capture.copy_to_target(&frame, frame_seq, measure) {
            Ok(copy) => copy,
            Err(error) => return Err(self.copy_fault(frame_seq, &error)),
        };
        // One GPU copy time per new frame. A sample that is not ready yet waits for the next frame.
        let gpu_copy_ms = self
            .capture
            .try_take_gpu_copy_sample()
            .map(|sample| sample.gpu_copy_ms);

        let inference_start = QpcClock::now();
        let ticket = match self.model.preprocess.submit(
            frame_seq,
            &self.model.frame.capture_ready_fence,
            &self.model.frame.reuse_fence,
            reuse_completed,
            measure,
        ) {
            Ok(ticket) => ticket,
            Err(error) => {
                let stage = self.model.preprocess.fault_stage();
                return Err(self.gpu_fault(stage, frame_seq, frame_seq, &error));
            }
        };
        self.last_submitted = frame_seq;

        // The ONNX Runtime run is checked after the collect, so that the GPU work of the frame is complete.
        let run = self.model.backend.run();
        let gpu_preprocess_ms = match self
            .model
            .preprocess
            .collect(ticket, &self.model.frame.reuse_fence)
        {
            Ok(value) => value,
            Err(error) => {
                return Err(self.gpu_fault(
                    GpuFaultStage::PreprocessCollect,
                    frame_seq,
                    ticket.completion_value,
                    &error,
                ));
            }
        };
        let output = match run {
            Ok(output) => output,
            Err(error) if self.model.gpu.device_lost() => {
                return Err(self.gpu_fault(
                    GpuFaultStage::BackendRun,
                    frame_seq,
                    ticket.completion_value,
                    &error,
                ));
            }
            Err(error) => {
                return Err(PipelineFault {
                    kind: FaultKind::Invalid,
                    message: format!("inference failed: {error:#}"),
                    report: None,
                });
            }
        };

        Ok(FrameOutcome::NewFrame(ProcessedFrame {
            frame_seq,
            accumulated_frames: frame.accumulated_frames(),
            desktop_size: frame.desktop_size(),
            times: FrameTimes {
                present: frame.present_qpc(),
                attempt_start,
                acquired: acquired_qpc,
                copy_submitted: copy.copy_submitted_qpc,
                ready: copy.ready_qpc,
                inference_start,
                inference_done: output.inference_done_qpc,
                postprocess_done: output.postprocess_done_qpc,
            },
            gpu_preprocess_ms,
            gpu_copy_ms,
            selected: output.selection.nearest,
            candidates: output.selection.candidates,
        }))
    }

    pub(crate) fn set_confidence_threshold(&mut self, value: ConfidenceThreshold) {
        self.model.backend.set_confidence_threshold(value);
    }

    /// Called at the start of a run: the first frames are measured again.
    pub(crate) fn reset_profiling(&mut self) {
        self.sampler.reset();
        self.capture.reset_gpu_profiler();
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn prepare_id(&self) -> &str {
        &self.model.prepare_id
    }

    pub(crate) fn display(&self) -> &DisplayInfo {
        self.capture.info()
    }

    pub(crate) fn input_size(&self) -> (usize, usize) {
        self.model.input_size
    }

    pub(crate) fn precision(&self) -> TensorPrecision {
        self.model.precision
    }

    pub(crate) fn ep_assignment(&self) -> &Arc<EpAssignmentState> {
        &self.model.ep_assignment
    }

    /// Reads the reuse fence once for a frame. Fails when the GPU is still reading the shared
    /// frame or the device is removed. No capture was made for the frame.
    fn check_reuse_fence(&self) -> Result<u64, PipelineFault> {
        let reuse_completed = self.model.frame.reuse_completed();
        if reuse_completed == u64::MAX {
            return Err(self.reuse_fault(&anyhow::anyhow!(
                "D3D12 completion fence reports device removed"
            )));
        }
        if reuse_completed < self.last_submitted {
            return Err(self.reuse_fault(&anyhow::anyhow!(
                "shared frame is still being read by GPU: required={} completed={reuse_completed}",
                self.last_submitted
            )));
        }
        Ok(reuse_completed)
    }

    fn reuse_fault(&self, error: &anyhow::Error) -> PipelineFault {
        PipelineFault {
            kind: FaultKind::Poisoned {
                device_lost: self.model.gpu.device_lost(),
            },
            message: format!("GPU frame reuse failed: {error:#}"),
            report: None,
        }
    }

    /// The capture failed. A lost device poisons the transport; anything else suspends the pipeline.
    fn capture_fault(&self, error: &anyhow::Error) -> PipelineFault {
        let device_lost = self.capture.device_lost();
        PipelineFault {
            kind: if device_lost {
                FaultKind::Poisoned { device_lost: true }
            } else {
                FaultKind::Invalid
            },
            message: format!("DXGI capture failed: {error:#}"),
            report: None,
        }
    }

    fn copy_fault(&self, frame_seq: u64, error: &anyhow::Error) -> PipelineFault {
        let mut report = self.report(
            self.capture.last_fault_stage(),
            frame_seq,
            Some(self.last_submitted),
            error,
        );
        report.d3d11_removed_reason = self.capture.device_removed_reason();
        PipelineFault {
            kind: FaultKind::Poisoned {
                device_lost: self.capture.device_lost() || self.model.gpu.device_lost(),
            },
            message: format!("GPU capture copy failed: {error:#}"),
            report: Some(Box::new(report)),
        }
    }

    /// A fault in the GPU work of one frame. The GPU transport is poisoned, and the report names the fences.
    fn gpu_fault(
        &self,
        stage: GpuFaultStage,
        frame_seq: u64,
        reuse_value: u64,
        error: &anyhow::Error,
    ) -> PipelineFault {
        let report = self.report(stage, frame_seq, Some(reuse_value), error);
        PipelineFault {
            kind: FaultKind::Poisoned {
                device_lost: self.model.gpu.device_lost(),
            },
            message: format!("GPU pipeline failed: {error:#}"),
            report: Some(Box::new(report)),
        }
    }

    fn report(
        &self,
        stage: GpuFaultStage,
        frame_seq: u64,
        reuse_value: Option<u64>,
        error: &anyhow::Error,
    ) -> GpuFaultReport {
        collect_gpu_fault_report(
            &self.model.gpu,
            &FaultSite {
                pipeline_id: self.id,
                prepare_id: &self.model.prepare_id,
                frame_seq: Some(frame_seq),
                ready_value: Some(frame_seq),
                stage,
            },
            FenceProgress {
                reuse_value,
                ready_completed: Some(self.model.frame.capture_ready_completed()),
                reuse_completed: Some(self.model.frame.reuse_completed()),
            },
            error,
        )
    }
}
