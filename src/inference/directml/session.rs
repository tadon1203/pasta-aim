use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use ort::{
    memory::{AllocationDevice, Allocator, AllocatorType, MemoryInfo, MemoryType},
    session::{IoBinding, Session, builder::SessionBuilder},
    value::{DynValue, Outlet, PrimitiveTensorElementType, Tensor},
};
use windows::Win32::{
    AI::MachineLearning::DirectML::{DML_CREATE_DEVICE_FLAG_NONE, DMLCreateDevice, IDMLDevice},
    Graphics::Direct3D12::{
        D3D12_RESOURCE_DIMENSION_BUFFER, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, ID3D12Resource,
    },
};

use crate::{
    graphics::device::GpuDeviceContext,
    inference::{
        EpAssignmentState, ModelInputSpec, TensorPrecision, ep_assignment,
        postprocess::{Selection, select_nearest},
    },
    settings::ConfidenceThreshold,
    timing::QpcClock,
};

use super::{append_dml1, d3d12_resource_from_dml_tensor};

static NEXT_PREPARE_ID: AtomicU64 = AtomicU64::new(1);

/// The detections of one ONNX Runtime run, with the QPC ticks of its two stage boundaries.
pub struct InferenceOutput {
    pub selection: Selection,
    /// After the ONNX Runtime run.
    pub inference_done_qpc: i64,
    /// After the postprocess.
    pub postprocess_done_qpc: i64,
}

/// The DirectML-allocated input tensor, in the model's input precision.
enum InputTensor {
    Fp16(Tensor<half::f16>),
    Fp32(Tensor<f32>),
}

impl InputTensor {
    /// Allocates the tensor and returns it with its backing D3D12 buffer.
    fn create(
        allocator: &Allocator,
        spec: ModelInputSpec,
        prepare_id: &str,
    ) -> Result<(Self, ID3D12Resource)> {
        match spec.precision {
            TensorPrecision::Fp16 => {
                let (tensor, resource) = allocate_input::<half::f16>(allocator, spec, prepare_id)?;
                Ok((Self::Fp16(tensor), resource))
            }
            TensorPrecision::Fp32 => {
                let (tensor, resource) = allocate_input::<f32>(allocator, spec, prepare_id)?;
                Ok((Self::Fp32(tensor), resource))
            }
        }
    }

    fn bind(&self, binding: &mut IoBinding, name: String) -> Result<()> {
        match self {
            Self::Fp16(tensor) => binding.bind_input(name, tensor)?,
            Self::Fp32(tensor) => binding.bind_input(name, tensor)?,
        }
        Ok(())
    }
}

fn allocate_input<T: PrimitiveTensorElementType + std::fmt::Debug>(
    allocator: &Allocator,
    spec: ModelInputSpec,
    prepare_id: &str,
) -> Result<(Tensor<T>, ID3D12Resource)> {
    let mut tensor = Tensor::<T>::new(allocator, [1, 3, spec.height, spec.width])?;
    let input_bytes = spec
        .width
        .checked_mul(spec.height)
        .and_then(|pixels| pixels.checked_mul(3))
        .and_then(|elements| spec.precision.packed_words(elements))
        .and_then(|words| words.checked_mul(4))
        .context("DirectML input byte size overflowed")?;
    // SAFETY: the tensor was just allocated from this session's DirectML allocator.
    let resource = unsafe { d3d12_resource_from_dml_tensor(allocator, &mut tensor, prepare_id) }?;
    validate_input_resource(&resource, input_bytes)?;
    Ok((tensor, resource))
}

/// ONNX Runtime / `DirectML` session state. Device, queue, shared frames, and
/// preprocessing are owned by the enclosing Pipeline.
///
/// Field order is drop order: tensors must disappear before their `DirectML`
/// allocator and session.
pub struct OrtDirectMlBackend {
    binding: IoBinding,
    _input_tensor: InputTensor,
    input_resource: ID3D12Resource,
    _allocator: Allocator,
    session: Session,
    _dml_device: IDMLDevice,
    input_spec: ModelInputSpec,
    prepare_id: String,
    output_name: String,
    output_precision: TensorPrecision,
    /// Rows of the static `[1, rows, 6]` output, validated at load.
    output_rows: usize,
    confidence_threshold: ConfidenceThreshold,
    ep_assignment: EpAssignmentState,
}

impl OrtDirectMlBackend {
    pub fn prepare(path: &Path, gpu: &GpuDeviceContext, low_latency: bool) -> Result<Self> {
        let prepare_id = format!(
            "{}-{}",
            std::process::id(),
            NEXT_PREPARE_ID.fetch_add(1, Ordering::Relaxed)
        );
        let total_start = Instant::now();
        tracing::info!(prepare_id = %prepare_id, model = %path.display(), "directml_prepare_begin");
        let result = Self::prepare_inner(path, gpu, &prepare_id, low_latency);
        match result {
            Ok(backend) => {
                tracing::info!(prepare_id = %prepare_id, total_ms = total_start.elapsed().as_secs_f64() * 1000.0, "directml_prepare_complete");
                Ok(backend)
            }
            Err(error) => {
                tracing::error!(prepare_id = %prepare_id, error = %crate::logging::chain(&error), "directml_prepare_failed");
                Err(error)
            }
        }
    }

    fn prepare_inner(
        path: &Path,
        gpu: &GpuDeviceContext,
        prepare_id: &str,
        low_latency: bool,
    ) -> Result<Self> {
        if !path.is_file() {
            bail!("model does not exist: {}", path.display());
        }
        let mut dml_device = None;
        unsafe { DMLCreateDevice(gpu.device(), DML_CREATE_DEVICE_FLAG_NONE, &mut dml_device) }
            .context("failed to create IDMLDevice")?;
        let dml_device: IDMLDevice = dml_device.context("DMLCreateDevice returned no device")?;

        let mut builder =
            session_builder(low_latency).map_err(|error| anyhow::anyhow!(error.to_string()))?;
        // SAFETY: gpu owns the D3D12 device and its single compute queue for
        // longer than this session and both are passed to DML1 unchanged.
        unsafe { append_dml1(&mut builder, &dml_device, gpu.queue(), prepare_id) }?;
        let session_start = Instant::now();
        let session = builder.commit_from_file(path).with_context(|| {
            format!(
                "failed to load ONNX model with DirectML: {}",
                path.display()
            )
        })?;
        tracing::info!(
            prepare_id,
            duration_ms = session_start.elapsed().as_secs_f64() * 1000.0,
            "ort_session_created"
        );

        if session.inputs().len() != 1 || session.outputs().len() != 1 {
            bail!("expected exactly one model input and one model output");
        }
        let dims: &[i64] = session.inputs()[0]
            .dtype()
            .tensor_shape()
            .context("model input must be a tensor")?
            .as_ref();
        if dims.len() != 4 || dims[0] != 1 || dims[1] != 3 || dims[2] <= 0 || dims[3] <= 0 {
            bail!("expected static NCHW input [1, 3, H, W], got {dims:?}");
        }
        let input_spec = ModelInputSpec {
            width: usize::try_from(dims[3]).context("model input width")?,
            height: usize::try_from(dims[2]).context("model input height")?,
            precision: io_precision(&session.inputs()[0], "input")?,
        };
        let output_precision = io_precision(&session.outputs()[0], "output")?;
        let output_rows = validate_output_shape(&session.outputs()[0])?;
        validate_metadata(&session, input_spec)?;
        let input_name = session.inputs()[0].name().to_owned();
        let output_name = session.outputs()[0].name().to_owned();
        let ep_assignment = ep_assignment::read(&session).map_or_else(
            |error| crate::inference::EpAssignmentState::Unavailable(format!("{error:#}")),
            crate::inference::EpAssignmentState::Available,
        );

        let allocator = Allocator::new(
            &session,
            MemoryInfo::new(
                AllocationDevice::DIRECTML,
                0,
                AllocatorType::Device,
                MemoryType::Default,
            )?,
        )?;
        let (input_tensor, input_resource) =
            InputTensor::create(&allocator, input_spec, prepare_id)?;

        let mut binding = session.create_binding()?;
        input_tensor.bind(&mut binding, input_name)?;
        let cpu_memory = MemoryInfo::new(
            AllocationDevice::CPU,
            0,
            AllocatorType::Device,
            MemoryType::Default,
        )?;
        binding.bind_output_to_device(output_name.clone(), &cpu_memory)?;

        Ok(Self {
            binding,
            _input_tensor: input_tensor,
            input_resource,
            _allocator: allocator,
            session,
            _dml_device: dml_device,
            input_spec,
            prepare_id: prepare_id.to_owned(),
            output_name,
            output_precision,
            output_rows,
            confidence_threshold: ConfidenceThreshold::default(),
            ep_assignment,
        })
    }

    pub fn prepare_id(&self) -> &str {
        &self.prepare_id
    }

    pub fn input_spec(&self) -> ModelInputSpec {
        self.input_spec
    }

    pub fn ep_assignment(&self) -> &EpAssignmentState {
        &self.ep_assignment
    }

    pub fn input_resource(&self) -> &ID3D12Resource {
        &self.input_resource
    }

    pub fn set_confidence_threshold(&mut self, value: ConfidenceThreshold) {
        self.confidence_threshold = value;
    }

    pub fn run(&mut self) -> Result<InferenceOutput> {
        // The tensor was bound once during prepare and is populated by the
        // pipeline's D3D12 preprocess dispatch before this call.
        let outputs = self.session.run_binding(&self.binding)?;
        let inference_done_qpc = QpcClock::now();
        let threshold = self.confidence_threshold.ratio();
        let output = &outputs[self.output_name.as_str()];
        let selection = match self.output_precision {
            TensorPrecision::Fp16 => {
                detect::<half::f16>(output, self.input_spec, self.output_rows, threshold)?
            }
            TensorPrecision::Fp32 => {
                detect::<f32>(output, self.input_spec, self.output_rows, threshold)?
            }
        };
        Ok(InferenceOutput {
            selection,
            inference_done_qpc,
            postprocess_done_qpc: QpcClock::now(),
        })
    }
}

/// Shape and dtype were validated at load, so this only reads the rows.
fn detect<T: PrimitiveTensorElementType + Copy + Into<f32>>(
    output: &DynValue,
    spec: ModelInputSpec,
    rows: usize,
    confidence_threshold: f32,
) -> Result<Selection> {
    let (_, data) = output.try_extract_tensor::<T>()?;
    debug_assert!(data.len() >= rows * 6);
    Ok(select_nearest(
        data,
        rows,
        spec.width,
        spec.height,
        confidence_threshold,
    ))
}

fn session_builder(low_latency: bool) -> ort::Result<SessionBuilder> {
    let mut builder = Session::builder()?
        .with_memory_pattern(false)?
        .with_parallel_execution(false)?
        // DirectML does the work; extra ORT worker threads only add wakeups.
        .with_intra_threads(1)?
        .with_intra_op_spinning(false)?
        .with_inter_op_spinning(false)?
        .with_config_entry("session.record_ep_graph_assignment_info", "1")?;
    if low_latency {
        // Busy-wait for GPU completion instead of sleeping on an event: lower
        // tail latency at the cost of one fully used CPU core.
        builder = builder.with_config_entry("ep.dml.enable_cpu_sync_spinning", "1")?;
    }
    Ok(builder)
}

/// The output must be a static end-to-end `[1, N, 6]` (or `[N, 6]`) detection
/// list; returns `N`.
fn validate_output_shape(outlet: &Outlet) -> Result<usize> {
    let dims: &[i64] = outlet
        .dtype()
        .tensor_shape()
        .context("model output must be a tensor")?
        .as_ref();
    match dims {
        [1, rows, 6] | [rows, 6] if *rows > 0 => {
            usize::try_from(*rows).context("model output rows")
        }
        _ => bail!(
            "expected a static end-to-end output [1, N, 6] (x1, y1, x2, y2, score, class), got {dims:?};              export the model with NMS embedded and a fixed input size"
        ),
    }
}

/// Rejects models whose Ultralytics-style export metadata contradicts what the
/// pipeline assumes. Models without metadata are accepted; the shape checks
/// above are what the pipeline actually relies on.
fn validate_metadata(session: &Session, spec: ModelInputSpec) -> Result<()> {
    let Ok(metadata) = session.metadata() else {
        return Ok(());
    };
    let flag = |key: &str| {
        metadata
            .custom(key)
            .map(|value| value.trim().eq_ignore_ascii_case("true"))
    };
    if flag("dynamic") == Some(true) {
        bail!("model was exported with dynamic shapes (metadata dynamic=True); export it static");
    }
    if flag("end2end") == Some(false) && flag("nms") != Some(true) {
        bail!("model is not end-to-end (metadata end2end=False); export it with NMS embedded");
    }
    if let Some((height, width)) = metadata.custom("imgsz").as_deref().and_then(parse_imgsz)
        && (height, width) != (spec.height, spec.width)
    {
        bail!(
            "model metadata imgsz {height}x{width} does not match its input {}x{}",
            spec.height,
            spec.width
        );
    }
    Ok(())
}

/// Parses Ultralytics' `imgsz` metadata, e.g. `[640, 640]`, as `(height, width)`.
fn parse_imgsz(value: &str) -> Option<(usize, usize)> {
    let sizes: Vec<usize> = value
        .trim_matches(|c: char| matches!(c, '[' | ']' | '(' | ')'))
        .split(',')
        .map(|part| part.trim().parse().ok())
        .collect::<Option<_>>()?;
    match sizes[..] {
        [height, width] => Some((height, width)),
        _ => None,
    }
}

fn io_precision(outlet: &Outlet, role: &str) -> Result<TensorPrecision> {
    let element_type = outlet
        .dtype()
        .tensor_type()
        .with_context(|| format!("model {role} must be a tensor"))?;
    TensorPrecision::from_element_type(element_type).with_context(|| {
        format!("unsupported model {role} element type {element_type}; expected FP16 or FP32")
    })
}

fn validate_input_resource(resource: &ID3D12Resource, expected_input_bytes: usize) -> Result<()> {
    let desc = unsafe { resource.GetDesc() };
    if desc.Dimension != D3D12_RESOURCE_DIMENSION_BUFFER {
        bail!(
            "DirectML input resource must be a BUFFER, got {:?}",
            desc.Dimension
        );
    }
    if desc.Width < expected_input_bytes as u64 {
        bail!(
            "DirectML input resource is too small: expected at least {expected_input_bytes} bytes, got {}",
            desc.Width
        );
    }
    if desc.Flags.0 & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0 == 0 {
        bail!("DirectML input resource does not allow unordered access");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imgsz_metadata_is_height_then_width() {
        assert_eq!(parse_imgsz("[480, 640]"), Some((480, 640)));
        assert_eq!(parse_imgsz("(640, 640)"), Some((640, 640)));
    }

    #[test]
    fn unrecognized_imgsz_metadata_is_ignored() {
        assert_eq!(parse_imgsz("640"), None);
        assert_eq!(parse_imgsz("[a, b]"), None);
        assert_eq!(parse_imgsz(""), None);
    }
}
