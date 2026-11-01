//! The model half of a pipeline: the ONNX Runtime session, the preprocess, and the D3D12 side of the
//! shared frame. It is built and replaced as a unit.

use std::sync::Arc;

use anyhow::{Context, Result, bail};

use super::PipelineConfig;
use crate::{
    graphics::{
        DxgiCapture,
        device::GpuDeviceContext,
        preprocess::GpuPreprocessor,
        shared_frame::{CaptureFrame, InferenceFrame, create_shared_frame},
    },
    inference::{EpAssignmentState, TensorPrecision, directml::OrtDirectMlBackend},
};

/// Field order is drop order: the ONNX Runtime objects, the preprocessor, the D3D12 half of the
/// shared frame, and the D3D12 device last. Dropping waits for the queue first.
pub(super) struct ModelHalf {
    pub backend: OrtDirectMlBackend,
    pub preprocess: GpuPreprocessor,
    pub frame: InferenceFrame,
    pub gpu: GpuDeviceContext,
    pub prepare_id: String,
    pub input_size: (usize, usize),
    pub precision: TensorPrecision,
    pub ep_assignment: Arc<EpAssignmentState>,
}

impl Drop for ModelHalf {
    fn drop(&mut self) {
        // The queue must be idle before any of the fields below are released.
        if let Err(error) = self.gpu.wait_idle() {
            tracing::warn!(prepare_id = %self.prepare_id, error = %format_args!("{error:#}"), "inference_pipeline_shutdown_failed");
        }
    }
}

/// Builds the model half on a new D3D12 device, and the D3D11 half of the shared frame on `capture`'s device.
/// It binds nothing and changes no state.
pub(super) fn build(
    capture: &DxgiCapture,
    config: &PipelineConfig<'_>,
) -> Result<(ModelHalf, CaptureFrame)> {
    let display = capture.info();
    let gpu = GpuDeviceContext::new(display.adapter_luid)?;
    let mut backend = OrtDirectMlBackend::prepare(config.model, &gpu, config.low_latency)?;
    backend.set_confidence_threshold(config.confidence_threshold);
    let spec = backend.input_spec();
    if spec.width > display.width as usize || spec.height > display.height as usize {
        bail!("model input exceeds the selected display dimensions");
    }
    let (capture_frame, frame) =
        create_shared_frame(&gpu, &capture.transport_device(), spec.width, spec.height)
            .context("failed to create D3D11-owned shared frame")?;
    let preprocess = GpuPreprocessor::new(
        &gpu,
        &frame.resource,
        backend.input_resource(),
        spec.width,
        spec.height,
        spec.precision,
    )?;
    let prepare_id = backend.prepare_id().to_owned();
    let ep_assignment = Arc::new(backend.ep_assignment().clone());
    tracing::info!(
        pipeline_id = config.pipeline_id,
        input_width = spec.width,
        input_height = spec.height,
        precision = spec.precision.label(),
        "inference_pipeline_prepared"
    );
    let model = ModelHalf {
        backend,
        preprocess,
        frame,
        gpu,
        prepare_id,
        input_size: (spec.width, spec.height),
        precision: spec.precision,
        ep_assignment,
    };
    Ok((model, capture_frame))
}
