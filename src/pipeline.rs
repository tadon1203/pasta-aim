mod capture;
mod gpu;
mod inference;
mod preprocess;
mod readback;

use std::{
    path::Path,
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

pub(crate) use self::capture::AcquiredDesktopFrame;
pub(crate) use self::capture::CapturePoll;
pub use self::capture::DxgiCapture;
pub(crate) use self::gpu::GpuDeviceContext;
use self::{
    capture::CopyTimes,
    gpu::{CaptureFrame, InferenceFrame},
    inference::{InferenceOutput, ModelInputSpec, OrtDirectMlBackend},
    preprocess::GpuPreprocessor,
};
use crate::{
    clock::Clock,
    settings::{self, Confidence},
};

static INITIALIZED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
static GPU_DEBUG: AtomicBool = AtomicBool::new(false);

pub fn init() -> Result<()> {
    let initialized = INITIALIZED.get_or_init(|| {
        if std::env::var_os("NICE_GPU_DEBUG").is_some() {
            gpu::enable_debug_layer().map_err(|error| format!("{error:#}"))?;
            GPU_DEBUG.store(true, Ordering::Relaxed);
        }
        Ok(())
    });
    initialized
        .clone()
        .map_err(|error| anyhow!(error).context("failed to enable the GPU debug layer"))
}

pub fn init_thread() -> Result<()> {
    // SAFETY: the engine thread lives until the process ends, so MTA is never balanced.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .context("failed to initialize COM MTA on the engine thread")
}

pub fn displays() -> Result<Vec<DisplayInfo>> {
    gpu::enumerate_displays()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AdapterLuid {
    pub low: u32,
    pub high: i32,
}

impl From<u64> for AdapterLuid {
    fn from(value: u64) -> Self {
        Self {
            low: value as u32,
            high: ((value >> 32) as u32).cast_signed(),
        }
    }
}

impl From<AdapterLuid> for u64 {
    fn from(luid: AdapterLuid) -> Self {
        (u64::from(luid.high.cast_unsigned()) << 32) | u64::from(luid.low)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayInfo {
    pub adapter_index: u32,
    pub output_index: u32,
    pub adapter_luid: AdapterLuid,
    pub adapter_name: String,
    pub display_name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
}

impl DisplayInfo {
    pub fn selection(&self) -> settings::Display {
        settings::Display {
            adapter: u64::from(self.adapter_luid),
            output: self.output_index,
        }
    }

    pub fn label(&self) -> String {
        format!(
            "{} · {} · {}×{} @ {:.1} Hz",
            self.adapter_name, self.display_name, self.width, self.height, self.refresh_hz
        )
    }
}

pub fn center_crop_origin(desktop: (usize, usize), input: (usize, usize)) -> (usize, usize) {
    ((desktop.0 - input.0) / 2, (desktop.1 - input.1) / 2)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    Fp32,
    Fp16,
}

impl Precision {
    pub fn element_bytes(self) -> usize {
        match self {
            Self::Fp16 => std::mem::size_of::<half::f16>(),
            Self::Fp32 => std::mem::size_of::<f32>(),
        }
    }

    pub fn packed_words(self, elements: usize) -> Option<usize> {
        Some(elements.checked_mul(self.element_bytes())?.div_ceil(4))
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Fp16 => "FP16",
            Self::Fp32 => "FP32",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CpuNodes {
    ops: Vec<(String, usize)>,
}

impl CpuNodes {
    pub fn from_operators(operators: impl IntoIterator<Item = String>) -> Self {
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        for operator in operators {
            *counts.entry(operator).or_default() += 1;
        }
        let mut ops: Vec<(String, usize)> = counts.into_iter().collect();
        ops.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
        Self { ops }
    }

    pub fn total(&self) -> usize {
        self.ops.iter().map(|&(_, count)| count).sum()
    }

    pub fn summary(&self) -> String {
        if self.ops.is_empty() {
            return "0".to_owned();
        }
        let ops: Vec<String> = self
            .ops
            .iter()
            .map(|(operator, count)| format!("{operator} ×{count}"))
            .collect();
        format!("{}: {}", self.total(), ops.join(", "))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Detection {
    pub center_x: f32,
    pub center_y: f32,
    pub box_width: f32,
    pub box_height: f32,
    pub confidence: f32,
    pub class_id: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct Times {
    pub present: i64,
    pub attempt_start: i64,
    pub acquired: i64,
    pub copy_submitted: i64,
    pub ready: i64,
    pub inference_start: i64,
    pub inference_done: i64,
    pub postprocess_done: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GpuFaultStage {
    CaptureAcquire,
    CaptureCopy,
    CaptureReadySignal,
    D3D12ReadyWait,
    PreprocessSubmit,
    PreprocessCollect,
    BackendRun,
    ReuseSignal,
    DeviceRemoved,
}

impl GpuFaultStage {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CaptureAcquire => "capture_acquire",
            Self::CaptureCopy => "capture_copy",
            Self::CaptureReadySignal => "capture_ready_signal",
            Self::D3D12ReadyWait => "d3d12_ready_wait",
            Self::PreprocessSubmit => "preprocess_submit",
            Self::PreprocessCollect => "preprocess_collect",
            Self::BackendRun => "backend_run",
            Self::ReuseSignal => "reuse_signal",
            Self::DeviceRemoved => "device_removed",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub confidence: Confidence,
    pub low_latency: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub desktop_size: (usize, usize),
    pub times: Times,
    pub gpu_preprocess_ms: Option<f64>,
    pub gpu_copy_ms: Option<f64>,
    pub selected: Option<Detection>,
}

#[derive(Debug, thiserror::Error)]
pub enum Fault {
    #[error("GPU failed: {0:#}")]
    Gpu(anyhow::Error),
    #[error("{0:#}")]
    Other(anyhow::Error),
}

struct Inferred {
    start: i64,
    gpu_preprocess_ms: Option<f64>,
    output: InferenceOutput,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Field order is drop order: the D3D12 device goes last.
struct ModelHalf {
    backend: OrtDirectMlBackend,
    preprocess: GpuPreprocessor,
    frame: InferenceFrame,
    gpu: GpuDeviceContext,
    prepare_id: String,
    input_size: (usize, usize),
    precision: Precision,
    cpu_nodes: Option<CpuNodes>,
}

impl Drop for ModelHalf {
    fn drop(&mut self) {
        // The queue must be idle before any of the fields below are released.
        if let Err(error) = self.gpu.wait_idle() {
            tracing::warn!(prepare_id = %self.prepare_id, error = %format_args!("{error:#}"), "inference_pipeline_shutdown_failed");
        }
    }
}

pub(crate) struct Pipeline {
    // Drop order: the model half, then the D3D11 half of the shared frame, then the capture last.
    model: ModelHalf,
    capture_frame: CaptureFrame,
    capture: DxgiCapture,
    id: u64,
    frame_seq: u64,
    last_submitted: u64,
    sampler: ProfileSampler,
}

impl Pipeline {
    pub fn new(display: &settings::Display, model: &Path, options: Options) -> Result<Self> {
        let info = displays()?
            .into_iter()
            .find(|info| info.selection() == *display)
            .context("the selected display is not connected")?;
        let mut capture = DxgiCapture::new(info.adapter_index, info.output_index)?;
        let (model, capture_frame) = build_model(&mut capture, model, options)?;
        Ok(Self {
            model,
            capture_frame,
            capture,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            frame_seq: 0,
            last_submitted: 0,
            sampler: ProfileSampler::default(),
        })
    }

    pub fn set_model(&mut self, model: &Path, options: Options) -> Result<()> {
        let (new_model, capture_frame) = build_model(&mut self.capture, model, options)?;
        let old_model = std::mem::replace(&mut self.model, new_model);
        let old_frame = std::mem::replace(&mut self.capture_frame, capture_frame);
        drop(old_model);
        drop(old_frame);
        self.id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        self.frame_seq = 0;
        self.last_submitted = 0;
        Ok(())
    }

    pub fn process_next(&mut self, wait_ms: u32) -> Result<Option<Frame>, Fault> {
        let reuse_completed = self.check_reuse_fence()?;

        let attempt_start = Clock::now();
        let frame = match self.capture.acquire(wait_ms) {
            Ok(CapturePoll::Fresh(frame)) => frame,
            Ok(CapturePoll::NoDesktopUpdate) => return Ok(None),
            Err(error) => return Err(self.capture_fault(&error)),
        };
        let acquired_qpc = Clock::now();
        let measure = self.sampler.should_measure();
        let (copy, gpu_copy_ms) = self.copy_stage(&frame, measure)?;
        let inferred = self.infer_stage(reuse_completed, measure)?;

        Ok(Some(Frame {
            desktop_size: frame.desktop_size(),
            times: Times {
                present: frame.present_qpc(),
                attempt_start,
                acquired: acquired_qpc,
                copy_submitted: copy.copy_submitted_qpc,
                ready: copy.ready_qpc,
                inference_start: inferred.start,
                inference_done: inferred.output.inference_done_qpc,
                postprocess_done: inferred.output.postprocess_done_qpc,
            },
            gpu_preprocess_ms: inferred.gpu_preprocess_ms,
            gpu_copy_ms,
            selected: inferred.output.selection.nearest,
        }))
    }

    fn copy_stage(
        &mut self,
        frame: &AcquiredDesktopFrame,
        measure: bool,
    ) -> Result<(CopyTimes, Option<f64>), Fault> {
        let frame_seq = self
            .frame_seq
            .checked_add(1)
            .expect("frame sequence exhausted");
        self.frame_seq = frame_seq;
        let copy = self
            .capture
            .copy_to_target(frame, frame_seq, measure)
            .map_err(|error| self.copy_fault(&error))?;
        let gpu_copy_ms = self
            .capture
            .try_take_gpu_copy_sample()
            .map(|sample| sample.gpu_copy_ms);
        Ok((copy, gpu_copy_ms))
    }

    fn infer_stage(&mut self, reuse_completed: u64, measure: bool) -> Result<Inferred, Fault> {
        let frame_seq = self.frame_seq;
        let start = Clock::now();
        let ticket = self
            .model
            .preprocess
            .submit(
                frame_seq,
                &self.model.frame.capture_ready_fence,
                &self.model.frame.reuse_fence,
                reuse_completed,
                measure,
            )
            .map_err(|error| {
                let stage = self.model.preprocess.fault_stage();
                self.fault(stage, "GPU pipeline failed", &error)
            })?;
        self.last_submitted = frame_seq;

        // The ONNX Runtime run is checked after the collect, so that the GPU work of the frame is complete.
        let run = self.model.backend.run();
        let gpu_preprocess_ms = self
            .model
            .preprocess
            .collect(ticket, &self.model.frame.reuse_fence)
            .map_err(|error| {
                self.fault(
                    GpuFaultStage::PreprocessCollect,
                    "GPU pipeline failed",
                    &error,
                )
            })?;
        let output =
            run.map_err(|error| self.fault(GpuFaultStage::BackendRun, "inference failed", &error))?;
        Ok(Inferred {
            start,
            gpu_preprocess_ms,
            output,
        })
    }

    pub fn set_confidence(&mut self, value: Confidence) {
        self.model.backend.set_confidence_threshold(value);
    }

    pub fn reset_timing(&mut self) {
        self.sampler.reset();
        self.capture.reset_gpu_profiler();
    }

    pub fn display(&self) -> &DisplayInfo {
        self.capture.info()
    }

    pub fn input_size(&self) -> (u32, u32) {
        let (width, height) = self.model.input_size;
        (width as u32, height as u32)
    }

    pub fn precision(&self) -> Precision {
        self.model.precision
    }

    pub fn cpu_nodes(&self) -> Option<&CpuNodes> {
        self.model.cpu_nodes.as_ref()
    }

    pub fn graph_capture(&self) -> bool {
        self.model.backend.graph_capture()
    }

    fn check_reuse_fence(&self) -> Result<u64, Fault> {
        let reuse_completed = self.model.frame.reuse_completed();
        if gpu::fence_reports_removal(reuse_completed) {
            let error = anyhow!("D3D12 completion fence reports device removed");
            return Err(self.fault(
                GpuFaultStage::DeviceRemoved,
                "GPU frame reuse failed",
                &error,
            ));
        }
        if reuse_completed < self.last_submitted {
            let error = anyhow!(
                "shared frame is still being read by GPU: required={} completed={reuse_completed}",
                self.last_submitted
            );
            return Err(self.fault(GpuFaultStage::ReuseSignal, "GPU frame reuse failed", &error));
        }
        Ok(reuse_completed)
    }

    fn capture_fault(&self, error: &anyhow::Error) -> Fault {
        self.fault(GpuFaultStage::CaptureAcquire, "DXGI capture failed", error)
    }

    fn copy_fault(&self, error: &anyhow::Error) -> Fault {
        self.fault(
            self.capture.last_fault_stage(),
            "GPU capture copy failed",
            error,
        )
    }

    /// A DXGI acquire failure and an inference failure are not GPU transport faults, so a rebuild does not help.
    fn fault(&self, stage: GpuFaultStage, message: &str, error: &anyhow::Error) -> Fault {
        if GPU_DEBUG.load(Ordering::Relaxed) {
            gpu::log_fault(
                &self.model.gpu,
                self.capture.device_removed_reason(),
                self.id,
                stage.as_str(),
                error,
            );
        }
        let error = anyhow!("{message}: {error:#}");
        match stage {
            GpuFaultStage::CaptureAcquire | GpuFaultStage::BackendRun => Fault::Other(error),
            _ => Fault::Gpu(error),
        }
    }
}

/// Binds the shared frame to `capture` as the last step, so a failure leaves `capture` unchanged.
fn build_model(
    capture: &mut DxgiCapture,
    model: &Path,
    options: Options,
) -> Result<(ModelHalf, CaptureFrame)> {
    let display = capture.info();
    let gpu = GpuDeviceContext::new(display.adapter_luid)?;
    let mut backend = OrtDirectMlBackend::prepare(model, &gpu, options.low_latency)?;
    backend.set_confidence_threshold(options.confidence);
    let spec: ModelInputSpec = backend.input_spec();
    if spec.width > display.width as usize || spec.height > display.height as usize {
        anyhow::bail!("model input exceeds the selected display dimensions");
    }
    let (capture_frame, frame) =
        gpu::create_shared_frame(&gpu, &capture.transport_device(), spec.width, spec.height)
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
    let cpu_nodes = backend.cpu_nodes().cloned();
    tracing::info!(
        model = %model.display(),
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
        cpu_nodes,
    };
    capture.bind_copy_target(&capture_frame, model.input_size)?;
    Ok((model, capture_frame))
}

#[derive(Debug, Default)]
struct ProfileSampler {
    frames: u64,
}

const COLD_FRAMES: u64 = 16;
const STEADY_STRIDE: u64 = 16;

impl ProfileSampler {
    fn reset(&mut self) {
        self.frames = 0;
    }

    fn should_measure(&mut self) -> bool {
        let frame = self.frames;
        self.frames = self.frames.saturating_add(1);
        frame < COLD_FRAMES || (frame - COLD_FRAMES).is_multiple_of(STEADY_STRIDE)
    }
}

#[cfg(test)]
impl Detection {
    pub(crate) fn for_test(center_x: f32, center_y: f32, class_id: i32) -> Self {
        Self {
            center_x,
            center_y,
            box_width: 10.0,
            box_height: 10.0,
            confidence: 0.9,
            class_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes(operators: &[&str]) -> CpuNodes {
        CpuNodes::from_operators(operators.iter().map(|&operator| operator.to_owned()))
    }

    #[test]
    fn cpu_nodes_are_counted_by_operator_with_the_most_frequent_first() {
        let cpu = nodes(&["Shape", "Gather", "Shape", "Gather", "Gather", "Cast"]);
        assert_eq!(cpu.total(), 6);
        assert_eq!(cpu.summary(), "6: Gather ×3, Shape ×2, Cast ×1");
    }

    #[test]
    fn cpu_nodes_with_equal_counts_are_in_name_order() {
        assert_eq!(
            nodes(&["Slice", "Add", "Mul"]).summary(),
            "3: Add ×1, Mul ×1, Slice ×1"
        );
    }

    #[test]
    fn no_cpu_nodes_are_zero() {
        assert_eq!(CpuNodes::default().total(), 0);
        assert_eq!(CpuNodes::default().summary(), "0");
    }
}
