use std::path::Path;

use anyhow::{Context, Result};
use windows::Win32::Graphics::Direct3D11::ID3D11Device4;

use crate::{
    graphics::{
        device::GpuDeviceContext,
        frame_mailbox::{FramePublisher, GPU_FRAME_SLOTS, GpuFrameLease},
        gpu_diagnostics::{
            FenceProgress, GpuFaultReport, GpuFaultStage, collect_gpu_fault_report,
            d3d12_device_lost,
        },
        preprocess::GpuPreprocessor,
        shared_frame_pool::{CaptureFramePool, InferenceFramePool, create_shared_frame_pool},
    },
    inference::{BackendInfo, BackendRunResult, Detection, InferenceBackend, ModelInputSpec},
    settings::ConfidenceThreshold,
};

use super::directml::OrtDirectMlBackend;

pub struct InferenceResult {
    pub detection: Option<Detection>,
    pub ort_run_wall_ms: f64,
    pub postprocess_ms: f64,
    pub gpu_preprocess_ms: f64,
}

pub struct InferencePipeline {
    gpu: Option<GpuDeviceContext>,
    frames: Option<InferenceFramePool>,
    preprocess: Option<GpuPreprocessor>,
    backend: Option<Box<dyn InferenceBackend>>,
    fault_report: Option<GpuFaultReport>,
    prepare_id: String,
    accepts_frames: bool,
}

pub struct PreparedPipeline {
    pub pipeline: InferencePipeline,
    pub capture_frames: CaptureFramePool,
    pub ep_assignment: crate::inference::EpAssignmentState,
}

pub fn prepare_directml_pipeline(
    model: &Path,
    adapter_luid: crate::inference::AdapterLuid,
    capture_device: &ID3D11Device4,
    publisher: FramePublisher,
    pipeline_id: u64,
) -> Result<PreparedPipeline> {
    let gpu = GpuDeviceContext::new(adapter_luid)?;
    let backend = OrtDirectMlBackend::prepare(model, &gpu)?;
    let input_spec = backend.input_spec();
    let backend_info = backend.info().clone();
    let prepare_id = backend.prepare_id().to_owned();
    let ep_assignment = backend.ep_assignment();
    let (capture_frames, frames) = create_shared_frame_pool(
        &gpu,
        capture_device,
        publisher.clone(),
        input_spec.width,
        input_spec.height,
    )
    .context("failed to create D3D12-owned shared frame pool")?;
    let input_resource = backend.input_resource()?;
    let preprocess = GpuPreprocessor::new(
        &gpu,
        &frames.resources,
        input_resource,
        input_spec.width,
        input_spec.height,
        input_spec.precision,
    )?;
    publisher.set_pipeline_identity(pipeline_id, prepare_id.clone());
    let mut pipeline = InferencePipeline {
        gpu: Some(gpu),
        frames: Some(frames),
        preprocess: Some(preprocess),
        backend: Some(Box::new(backend)),
        fault_report: None,
        prepare_id,
        accepts_frames: true,
    };
    pipeline.set_confidence_threshold(ConfidenceThreshold::default());
    tracing::info!(pipeline_id, backend = %backend_info.name, input_width = input_spec.width, input_height = input_spec.height, precision = input_spec.precision.label(), "inference_pipeline_prepared");
    Ok(PreparedPipeline {
        pipeline,
        capture_frames,
        ep_assignment,
    })
}

impl InferencePipeline {
    pub fn info(&self) -> &BackendInfo {
        self.backend
            .as_ref()
            .expect("backend exists before shutdown")
            .info()
    }

    pub fn input_spec(&self) -> ModelInputSpec {
        self.backend
            .as_ref()
            .expect("backend exists before shutdown")
            .input_spec()
    }

    pub fn prepare_id(&self) -> &str {
        &self.prepare_id
    }

    pub fn set_confidence_threshold(&mut self, threshold: ConfidenceThreshold) {
        if let Some(backend) = self.backend.as_mut() {
            backend.set_confidence_threshold(threshold);
        }
    }

    pub fn infer(&mut self, frame: &mut GpuFrameLease) -> Result<InferenceResult> {
        if !self.accepts_frames {
            anyhow::bail!("inference pipeline is shutting down");
        }
        let gpu = self.gpu.as_ref().context("GPU device was shut down")?;
        let frames = self
            .frames
            .as_ref()
            .context("inference frame pool was shut down")?;
        let meta = *frame.metadata();
        if frame.slot_id() >= GPU_FRAME_SLOTS {
            anyhow::bail!("invalid GPU frame slot {}", frame.slot_id());
        }
        let submit_result = self
            .preprocess
            .as_mut()
            .context("preprocessor was shut down")?
            .submit(frame, &frames.capture_ready_fence, &frames.reuse_fence);
        let ticket = match submit_result {
            Ok(ticket) => ticket,
            Err(error) => {
                let stage = self
                    .preprocess
                    .as_ref()
                    .map(GpuPreprocessor::fault_stage)
                    .unwrap_or(GpuFaultStage::PreprocessSubmit);
                self.fault_report = Some(Self::save_fault_report(
                    gpu,
                    frames,
                    meta,
                    Some(frame.reuse_value()),
                    stage,
                    &error,
                ));
                return Err(error);
            }
        };
        let run_result = self
            .backend
            .as_mut()
            .context("inference backend was shut down")?
            .run();
        let collect_result = self
            .preprocess
            .as_mut()
            .context("preprocessor was shut down")?
            .collect(ticket, &frames.reuse_fence);
        let gpu_preprocess_ms = match collect_result {
            Ok(value) => value,
            Err(error) => {
                self.fault_report = Some(Self::save_fault_report(
                    gpu,
                    frames,
                    meta,
                    Some(ticket.completion_value),
                    GpuFaultStage::PreprocessCollect,
                    &error,
                ));
                return Err(error);
            }
        };
        match run_result {
            Ok(BackendRunResult {
                detection,
                ort_run_wall_ms,
                postprocess_ms,
            }) => Ok(InferenceResult {
                detection,
                ort_run_wall_ms,
                postprocess_ms,
                gpu_preprocess_ms,
            }),
            Err(error) => {
                if self.device_lost() {
                    self.fault_report = Some(Self::save_fault_report(
                        gpu,
                        frames,
                        meta,
                        Some(ticket.completion_value),
                        GpuFaultStage::BackendRun,
                        &error,
                    ));
                }
                Err(error)
            }
        }
    }

    fn save_fault_report(
        gpu: &GpuDeviceContext,
        frames: &InferenceFramePool,
        meta: crate::graphics::GpuFrameMetadata,
        reuse_value: Option<u64>,
        stage: GpuFaultStage,
        error: &anyhow::Error,
    ) -> GpuFaultReport {
        let report = collect_gpu_fault_report(
            gpu,
            &frames.mailbox,
            meta,
            stage,
            FenceProgress {
                reuse_value,
                ready_completed: Some(unsafe { frames.capture_ready_fence.GetCompletedValue() }),
                reuse_completed: Some(unsafe { frames.reuse_fence.GetCompletedValue() }),
            },
            error,
        );
        frames.mailbox.poison();
        report
    }

    pub fn take_fault_report(&mut self) -> Option<GpuFaultReport> {
        self.fault_report.take()
    }

    pub fn device_lost(&self) -> bool {
        self.gpu
            .as_ref()
            .is_some_and(|gpu| d3d12_device_lost(gpu.device()))
    }

    pub fn shutdown(&mut self) -> Result<()> {
        self.accepts_frames = false;
        let wait_result = self
            .gpu
            .as_mut()
            .map(GpuDeviceContext::wait_idle)
            .unwrap_or(Ok(()));
        if let Some(mut backend) = self.backend.take() {
            backend.shutdown();
            drop(backend);
        }
        self.preprocess.take();
        self.frames.take();
        self.gpu.take();
        wait_result
    }
}

impl Drop for InferencePipeline {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            tracing::warn!(prepare_id = %self.prepare_id, error = %format!("{error:#}"), "inference_pipeline_shutdown_failed");
        }
    }
}
