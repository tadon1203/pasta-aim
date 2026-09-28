use std::path::Path;

use anyhow::{Context, Result};
use windows::Win32::Graphics::Direct3D11::ID3D11Device4;

use crate::{
    graphics::{
        device::GpuDeviceContext,
        frame_mailbox::{FramePublisher, GPU_FRAME_SLOTS, GpuFrameLease, GpuFrameMetadata},
        gpu_diagnostics::{
            FenceProgress, GpuFaultReport, GpuFaultStage, collect_gpu_fault_report,
            d3d12_device_lost,
        },
        preprocess::GpuPreprocessor,
        shared_frame_pool::{CaptureFramePool, InferenceFramePool, create_shared_frame_pool},
    },
    inference::{AdapterLuid, Candidates, Detection, EpAssignmentState, ModelInputSpec},
    settings::ConfidenceThreshold,
};

use super::directml::OrtDirectMlBackend;

pub struct InferenceResult {
    pub detection: Option<Detection>,
    pub candidates: Candidates,
    pub ort_run_wall_ms: f64,
    pub postprocess_ms: f64,
    pub gpu_preprocess_ms: Option<f64>,
}

/// Field order is drop order: ORT objects first, then the preprocessor and frame
/// pool, and the D3D12 device last.
pub struct InferencePipeline {
    backend: OrtDirectMlBackend,
    preprocess: GpuPreprocessor,
    frames: InferenceFramePool,
    gpu: GpuDeviceContext,
    fault_report: Option<GpuFaultReport>,
    prepare_id: String,
}

pub struct PreparedPipeline {
    pub pipeline: InferencePipeline,
    pub capture_frames: CaptureFramePool,
    pub ep_assignment: EpAssignmentState,
    pub input_spec: ModelInputSpec,
    pub prepare_id: String,
}

pub fn prepare_directml_pipeline(
    model: &Path,
    adapter_luid: AdapterLuid,
    capture_device: &ID3D11Device4,
    publisher: FramePublisher,
    pipeline_id: u64,
    confidence_threshold: ConfidenceThreshold,
    low_latency: bool,
) -> Result<PreparedPipeline> {
    let gpu = GpuDeviceContext::new(adapter_luid)?;
    let mut backend = OrtDirectMlBackend::prepare(model, &gpu, low_latency)?;
    backend.set_confidence_threshold(confidence_threshold);
    let input_spec = backend.input_spec();
    let prepare_id = backend.prepare_id().to_owned();
    let ep_assignment = backend.ep_assignment().clone();
    let (capture_frames, frames) = create_shared_frame_pool(
        &gpu,
        capture_device,
        publisher.clone(),
        input_spec.width,
        input_spec.height,
    )
    .context("failed to create D3D12-owned shared frame pool")?;
    let preprocess = GpuPreprocessor::new(
        &gpu,
        &frames.resources,
        backend.input_resource(),
        input_spec.width,
        input_spec.height,
        input_spec.precision,
    )?;
    publisher.set_pipeline_identity(pipeline_id, prepare_id.clone());
    tracing::info!(
        pipeline_id,
        input_width = input_spec.width,
        input_height = input_spec.height,
        precision = input_spec.precision.label(),
        "inference_pipeline_prepared"
    );
    Ok(PreparedPipeline {
        pipeline: InferencePipeline {
            backend,
            preprocess,
            frames,
            gpu,
            fault_report: None,
            prepare_id: prepare_id.clone(),
        },
        capture_frames,
        ep_assignment,
        input_spec,
        prepare_id,
    })
}

impl InferencePipeline {
    pub fn set_confidence_threshold(&mut self, threshold: ConfidenceThreshold) {
        self.backend.set_confidence_threshold(threshold);
    }

    pub fn infer(&mut self, frame: &mut GpuFrameLease) -> Result<InferenceResult> {
        let meta = *frame.metadata();
        if frame.slot_id() >= GPU_FRAME_SLOTS {
            anyhow::bail!("invalid GPU frame slot {}", frame.slot_id());
        }
        let ticket = match self.preprocess.submit(
            frame,
            &self.frames.capture_ready_fence,
            &self.frames.reuse_fence,
        ) {
            Ok(ticket) => ticket,
            Err(error) => {
                let stage = self.preprocess.fault_stage();
                self.record_fault(meta, Some(frame.reuse_value()), stage, &error);
                return Err(error);
            }
        };
        let run_result = self.backend.run();
        let gpu_preprocess_ms = match self.preprocess.collect(ticket, &self.frames.reuse_fence) {
            Ok(value) => value,
            Err(error) => {
                let value = Some(ticket.completion_value);
                self.record_fault(meta, value, GpuFaultStage::PreprocessCollect, &error);
                return Err(error);
            }
        };
        match run_result {
            Ok(mut result) => {
                result.gpu_preprocess_ms = gpu_preprocess_ms;
                Ok(result)
            }
            Err(error) => {
                if self.device_lost() {
                    let value = Some(ticket.completion_value);
                    self.record_fault(meta, value, GpuFaultStage::BackendRun, &error);
                }
                Err(error)
            }
        }
    }

    fn record_fault(
        &mut self,
        meta: GpuFrameMetadata,
        reuse_value: Option<u64>,
        stage: GpuFaultStage,
        error: &anyhow::Error,
    ) {
        let frames = &self.frames;
        let report = collect_gpu_fault_report(
            &self.gpu,
            &frames.mailbox,
            meta,
            stage,
            FenceProgress {
                reuse_value,
                // SAFETY: both fences are live COM objects owned by the frame pool.
                ready_completed: Some(unsafe { frames.capture_ready_fence.GetCompletedValue() }),
                reuse_completed: Some(unsafe { frames.reuse_fence.GetCompletedValue() }),
            },
            error,
        );
        frames.mailbox.poison();
        self.fault_report = Some(report);
    }

    pub fn take_fault_report(&mut self) -> Option<GpuFaultReport> {
        self.fault_report.take()
    }

    pub fn device_lost(&self) -> bool {
        d3d12_device_lost(self.gpu.device())
    }
}

impl Drop for InferencePipeline {
    fn drop(&mut self) {
        // The queue must be idle before any of the fields below are released.
        if let Err(error) = self.gpu.wait_idle() {
            tracing::warn!(prepare_id = %self.prepare_id, error = %format!("{error:#}"), "inference_pipeline_shutdown_failed");
        }
    }
}
