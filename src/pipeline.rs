//! The pipeline: one capture attempt to one postprocessed frame, and its replacement as a unit.
//!
//! The pipeline owns the capture, the shared frame, the preprocess, and the inference session.
//! The engine sees no fence, no texture, and no fence value. The `unsafe` code is in the
//! children: `gpu`, `capture`, `preprocess`, and `inference`.

mod capture;
mod gpu;
mod inference;
mod preprocess;

use std::{
    path::Path,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

#[cfg(test)]
pub(crate) use self::capture::AcquiredDesktopFrame;
pub use self::capture::DxgiCapture;
pub(crate) use self::capture::{CapturePoll, NoDesktopUpdateReason};
pub(crate) use self::gpu::GpuDeviceContext;
use self::{
    gpu::{CaptureFrame, InferenceFrame},
    inference::{ModelInputSpec, OrtDirectMlBackend},
    preprocess::GpuPreprocessor,
};
use crate::{clock::Clock, settings::Confidence};

// ---- Startup ----

static INITIALIZED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
static GPU_DEBUG: AtomicBool = AtomicBool::new(false);

/// Once at startup, before any device. GPU debug enables the D3D12 debug layer.
pub fn init(gpu_debug: bool) -> Result<()> {
    let initialized = INITIALIZED.get_or_init(|| {
        if gpu_debug {
            gpu::enable_debug_layer().map_err(|error| format!("{error:#}"))?;
            GPU_DEBUG.store(true, Ordering::Relaxed);
        }
        Ok(())
    });
    initialized
        .clone()
        .map_err(|error| anyhow!(error).context("failed to enable the GPU debug layer"))
}

/// Once at the start of the engine thread: COM MTA for that thread, never uninitialized.
pub fn init_thread() -> Result<()> {
    // SAFETY: the engine thread lives until the process ends, so MTA is never balanced.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .context("failed to initialize COM MTA on the engine thread")
}

/// Lists the displays of the system.
pub fn displays() -> Result<Vec<DisplayInfo>> {
    gpu::enumerate_displays()
}

// ---- Types ----

/// A LUID that the settings store as one number: `(high << 32) | low`.
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
    pub fn label(&self) -> String {
        format!(
            "{} · {} · {}×{} @ {:.1} Hz",
            self.adapter_name, self.display_name, self.width, self.height, self.refresh_hz
        )
    }
}

/// Top-left corner of the centered `input` region inside a `desktop`-sized frame.
pub fn center_crop_origin(desktop: (usize, usize), input: (usize, usize)) -> (usize, usize) {
    ((desktop.0 - input.0) / 2, (desktop.1 - input.1) / 2)
}

/// Floating point element type of a model's input tensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    Fp32,
    Fp16,
}

impl Precision {
    /// The size of one tensor element.
    pub fn element_bytes(self) -> usize {
        match self {
            Self::Fp16 => std::mem::size_of::<half::f16>(),
            Self::Fp32 => std::mem::size_of::<f32>(),
        }
    }

    /// Number of 32-bit words needed to hold `elements` tensor elements.
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

/// One detection in input-image pixels. `box_width` is kept: the tracker and the trace use it.
#[derive(Debug, Clone, Copy)]
pub struct Detection {
    pub center_x: f32,
    pub center_y: f32,
    pub box_width: f32,
    pub box_height: f32,
    pub confidence: f32,
    pub class_id: i32,
}

impl Detection {
    const ZERO: Self = Self {
        center_x: 0.0,
        center_y: 0.0,
        box_width: 0.0,
        box_height: 0.0,
        confidence: 0.0,
        class_id: 0,
    };
}

pub const MAX_DETECTIONS: usize = 16;

/// Up to `MAX_DETECTIONS` detections that passed the confidence filter, in detection order, and the
/// full count. Fixed-size, so the frame path does not allocate.
#[derive(Debug, Clone, Copy)]
pub struct Detections {
    items: [Detection; MAX_DETECTIONS],
    len: usize,
    total: usize,
}

impl Detections {
    pub const EMPTY: Self = Self {
        items: [Detection::ZERO; MAX_DETECTIONS],
        len: 0,
        total: 0,
    };

    /// Stores the detection while there is room, and always counts it.
    pub(crate) fn push(&mut self, detection: Detection) {
        if self.len < MAX_DETECTIONS {
            self.items[self.len] = detection;
            self.len += 1;
        }
        self.total += 1;
    }

    pub fn iter(&self) -> impl Iterator<Item = &Detection> {
        self.items[..self.len].iter()
    }

    /// How many passed the filter, including any beyond `MAX_DETECTIONS`.
    pub fn total(&self) -> usize {
        self.total
    }
}

/// The stage boundaries of one frame, as QPC ticks.
#[derive(Debug, Clone, Copy)]
pub struct FrameTimes {
    /// The Present time of the desktop update.
    pub present: i64,
    /// The capture attempt started, just before `AcquireNextFrame`.
    pub attempt_start: i64,
    /// The desktop image was acquired.
    pub acquired: i64,
    /// The copy to the shared texture was recorded.
    pub copy_submitted: i64,
    /// The capture-ready fence was signalled and flushed.
    pub ready: i64,
    /// Just before the preprocess is submitted.
    pub inference_start: i64,
    /// After the ONNX Runtime run.
    pub inference_done: i64,
    /// After the postprocess.
    pub postprocess_done: i64,
}

/// Why the engine dropped a frame that a pipeline produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuDropReason {
    StaleGeneration,
    PipelinePoisoned,
}

impl GpuDropReason {
    /// The name of the reason in the frame trace.
    pub const fn name(self) -> &'static str {
        match self {
            Self::StaleGeneration => "stale_generation",
            Self::PipelinePoisoned => "pipeline_poisoned",
        }
    }
}

/// The GPU stage in which a fault happened. The fault log names it.
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

// ---- Pipeline ----

/// What the engine asks of a pipeline when it is built or its model is replaced.
pub(crate) struct PipelineConfig<'a> {
    pub model: &'a Path,
    pub pipeline_id: u64,
    pub confidence_threshold: Confidence,
    pub low_latency: bool,
    pub qpc: Clock,
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
    pub detections: Detections,
}

// The frame is handed over by value, so the frame path does not allocate.
#[allow(clippy::large_enum_variant)]
pub(crate) enum FrameOutcome {
    NewFrame(ProcessedFrame),
    NoDesktopUpdate(NoDesktopUpdateReason),
}

/// The pipeline cannot be used any more. The engine suspends it until the user retries.
pub(crate) struct PipelineFault {
    pub message: String,
}

/// Field order is drop order: the ONNX Runtime objects, the preprocessor, the D3D12 half of the
/// shared frame, and the D3D12 device last. Dropping waits for the queue first.
struct ModelHalf {
    backend: OrtDirectMlBackend,
    preprocess: GpuPreprocessor,
    frame: InferenceFrame,
    gpu: GpuDeviceContext,
    prepare_id: String,
    input_size: (usize, usize),
    precision: Precision,
    cpu_nodes: Option<usize>,
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
    qpc: Clock,
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
        let (model, capture_frame) = match build_model(&capture, config) {
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
        let (model, capture_frame) = build_model(&self.capture, config)?;
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

        let attempt_start = Clock::now();
        let frame = match self.capture.acquire(wait_ms) {
            Ok(CapturePoll::Fresh(frame)) => frame,
            Ok(CapturePoll::NoDesktopUpdate(reason)) => {
                return Ok(FrameOutcome::NoDesktopUpdate(reason));
            }
            Err(error) => return Err(self.capture_fault(&error)),
        };
        let acquired_qpc = Clock::now();
        let frame_seq = self
            .frame_seq
            .checked_add(1)
            .expect("frame sequence exhausted");
        self.frame_seq = frame_seq;

        let measure = self.sampler.should_measure();
        let copy = match self.capture.copy_to_target(&frame, frame_seq, measure) {
            Ok(copy) => copy,
            Err(error) => return Err(self.copy_fault(&error)),
        };
        // One GPU copy time per new frame. A sample that is not ready yet waits for the next frame.
        let gpu_copy_ms = self
            .capture
            .try_take_gpu_copy_sample()
            .map(|sample| sample.gpu_copy_ms);

        let inference_start = Clock::now();
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
                return Err(self.fault(stage, "GPU pipeline failed", &error));
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
                return Err(self.fault(
                    GpuFaultStage::PreprocessCollect,
                    "GPU pipeline failed",
                    &error,
                ));
            }
        };
        let output = match run {
            Ok(output) => output,
            Err(error) => {
                return Err(self.fault(GpuFaultStage::BackendRun, "inference failed", &error));
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
            detections: output.selection.detections,
        }))
    }

    pub(crate) fn set_confidence_threshold(&mut self, value: Confidence) {
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

    pub(crate) fn precision(&self) -> Precision {
        self.model.precision
    }

    /// The nodes that ONNX Runtime assigned to the CPU. `None` when ORT does not report the assignment.
    pub(crate) fn cpu_nodes(&self) -> Option<usize> {
        self.model.cpu_nodes
    }

    /// Reads the reuse fence once for a frame. Fails when the GPU is still reading the shared
    /// frame or the device is removed. No capture was made for the frame.
    fn check_reuse_fence(&self) -> Result<u64, PipelineFault> {
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

    fn capture_fault(&self, error: &anyhow::Error) -> PipelineFault {
        self.fault(GpuFaultStage::CaptureAcquire, "DXGI capture failed", error)
    }

    fn copy_fault(&self, error: &anyhow::Error) -> PipelineFault {
        self.fault(
            self.capture.last_fault_stage(),
            "GPU capture copy failed",
            error,
        )
    }

    /// Logs the fault once when GPU debug is on, and returns it for the engine.
    fn fault(&self, stage: GpuFaultStage, message: &str, error: &anyhow::Error) -> PipelineFault {
        if GPU_DEBUG.load(Ordering::Relaxed) {
            gpu::log_fault(
                &self.model.gpu,
                self.capture.device_removed_reason(),
                self.id,
                stage.as_str(),
                error,
            );
        }
        PipelineFault {
            message: format!("{message}: {error:#}"),
        }
    }
}

/// Builds the model half on a new D3D12 device, and the D3D11 half of the shared frame on `capture`'s device.
/// It binds nothing and changes no state.
fn build_model(
    capture: &DxgiCapture,
    config: &PipelineConfig<'_>,
) -> Result<(ModelHalf, CaptureFrame)> {
    let display = capture.info();
    let gpu = GpuDeviceContext::new(display.adapter_luid)?;
    let mut backend = OrtDirectMlBackend::prepare(config.model, &gpu, config.low_latency)?;
    backend.set_confidence_threshold(config.confidence_threshold);
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
    let cpu_nodes = backend.cpu_nodes();
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
        cpu_nodes,
    };
    Ok((model, capture_frame))
}

/// Decides which frames get GPU timestamp queries.
///
/// The first frames are always measured (they include the cold run); after
/// that only every `STEADY_STRIDE`-th frame is, so steady-state frames skip the
/// query and resolve work.
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

    /// Counts a frame and returns whether it should be measured.
    fn should_measure(&mut self) -> bool {
        let frame = self.frames;
        self.frames = self.frames.saturating_add(1);
        frame < COLD_FRAMES || (frame - COLD_FRAMES).is_multiple_of(STEADY_STRIDE)
    }
}

#[cfg(test)]
impl Detection {
    /// A detection for the tests of the engine and the telemetry. The box is 10 by 10 pixels.
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
