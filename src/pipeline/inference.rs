use std::{
    ffi::{CStr, c_char},
    fmt::Debug,
    path::Path,
    slice,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use ort::{
    AsPointer,
    memory::{AllocationDevice, Allocator, AllocatorType, MemoryInfo, MemoryType},
    session::{IoBinding, Session, builder::SessionBuilder},
    sys,
    value::{Outlet, PrimitiveTensorElementType, Tensor, TensorElementType},
};
use windows::{
    Win32::{
        AI::MachineLearning::DirectML::{DML_CREATE_DEVICE_FLAG_NONE, DMLCreateDevice, IDMLDevice},
        Graphics::Direct3D12::{
            D3D12_RESOURCE_DIMENSION_BUFFER, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            ID3D12CommandQueue, ID3D12Resource,
        },
    },
    core::Interface,
};

use super::{Detection, GpuDeviceContext, Precision, readback::Readback};
use crate::{clock::Clock, settings::Confidence};

static NEXT_PREPARE_ID: AtomicU64 = AtomicU64::new(1);

const WARM_UP_RUNS: usize = 3;

const OUTPUT_COLUMNS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ModelInputSpec {
    pub width: usize,
    pub height: usize,
    pub precision: Precision,
}

pub(super) struct InferenceOutput {
    pub selection: Selection,
    pub inference_done_qpc: i64,
}

#[derive(Debug)]
pub(super) struct Selection {
    pub nearest: Option<Detection>,
}

enum DeviceTensor {
    Fp16(Tensor<half::f16>),
    Fp32(Tensor<f32>),
}

impl DeviceTensor {
    fn create(
        allocator: &Allocator,
        precision: Precision,
        shape: &[usize],
        role: &str,
        prepare_id: &str,
    ) -> Result<(Self, ID3D12Resource)> {
        match precision {
            Precision::Fp16 => {
                let (tensor, resource) =
                    allocate::<half::f16>(allocator, precision, shape, role, prepare_id)?;
                Ok((Self::Fp16(tensor), resource))
            }
            Precision::Fp32 => {
                let (tensor, resource) =
                    allocate::<f32>(allocator, precision, shape, role, prepare_id)?;
                Ok((Self::Fp32(tensor), resource))
            }
        }
    }

    fn bind_input(&self, binding: &mut IoBinding, name: String) -> Result<()> {
        match self {
            Self::Fp16(tensor) => binding.bind_input(name, tensor)?,
            Self::Fp32(tensor) => binding.bind_input(name, tensor)?,
        }
        Ok(())
    }

    fn bind_output(self, binding: &mut IoBinding, name: String) -> Result<()> {
        match self {
            Self::Fp16(tensor) => binding.bind_output(name, tensor)?,
            Self::Fp32(tensor) => binding.bind_output(name, tensor)?,
        }
        Ok(())
    }
}

fn allocate<T: PrimitiveTensorElementType + Debug>(
    allocator: &Allocator,
    precision: Precision,
    shape: &[usize],
    role: &str,
    prepare_id: &str,
) -> Result<(Tensor<T>, ID3D12Resource)> {
    let mut tensor = Tensor::<T>::new(allocator, shape)?;
    let bytes = shape
        .iter()
        .try_fold(1_usize, |elements, &size| elements.checked_mul(size))
        .and_then(|elements| precision.packed_words(elements))
        .and_then(|words| words.checked_mul(4))
        .with_context(|| format!("DirectML {role} byte size overflowed"))?;
    // SAFETY: the tensor was just allocated from this session's DirectML allocator.
    let resource = unsafe { d3d12_resource_from_dml_tensor(allocator, &mut tensor, prepare_id) }?;
    validate_tensor_resource(&resource, bytes, role)?;
    Ok((tensor, resource))
}

/// Field order is drop order: tensors go before their allocator and session.
pub(super) struct OrtDirectMlBackend {
    binding: IoBinding,
    _input_tensor: DeviceTensor,
    input_resource: ID3D12Resource,
    readback: Readback,
    _allocator: Allocator,
    session: Session,
    _dml_device: IDMLDevice,
    input_spec: ModelInputSpec,
    prepare_id: String,
    output_precision: Precision,
    output_rows: usize,
    output_values: Vec<f32>,
    confidence_threshold: Confidence,
    graph_capture: bool,
}

impl Drop for OrtDirectMlBackend {
    fn drop(&mut self) {
        // Drain the queue before the fields below are released.
        if let Err(error) = self.readback.drain() {
            tracing::warn!(prepare_id = %self.prepare_id, error = %format_args!("{error:#}"), "inference_backend_drain_failed");
        }
    }
}

impl OrtDirectMlBackend {
    pub(super) fn prepare(path: &Path, gpu: &GpuDeviceContext, low_latency: bool) -> Result<Self> {
        let prepare_id = format!(
            "{}-{}",
            std::process::id(),
            NEXT_PREPARE_ID.fetch_add(1, Ordering::Relaxed)
        );
        let total_start = Instant::now();
        tracing::info!(prepare_id = %prepare_id, model = %path.display(), "directml_prepare_begin");
        let result = Self::prepare_inner(path, gpu, &prepare_id, low_latency, true).or_else(|error| {
            tracing::warn!(prepare_id = %prepare_id, error = %format_args!("{error:#}"), "graph_capture_unavailable; loading the model without it");
            Self::prepare_inner(path, gpu, &prepare_id, low_latency, false)
        });
        match result {
            Ok(backend) => {
                tracing::info!(prepare_id = %prepare_id, graph_capture = backend.graph_capture, total_ms = total_start.elapsed().as_secs_f64() * 1000.0, "directml_prepare_complete");
                Ok(backend)
            }
            Err(error) => {
                tracing::error!(prepare_id = %prepare_id, error = %format_args!("{error:#}"), "directml_prepare_failed");
                Err(error)
            }
        }
    }

    fn prepare_inner(
        path: &Path,
        gpu: &GpuDeviceContext,
        prepare_id: &str,
        low_latency: bool,
        graph_capture: bool,
    ) -> Result<Self> {
        if !path.is_file() {
            bail!("model does not exist: {}", path.display());
        }
        let mut dml_device = None;
        unsafe { DMLCreateDevice(gpu.device(), DML_CREATE_DEVICE_FLAG_NONE, &mut dml_device) }
            .context("failed to create IDMLDevice")?;
        let dml_device: IDMLDevice = dml_device.context("DMLCreateDevice returned no device")?;

        let mut builder = session_builder(low_latency, graph_capture)
            .context("failed to create the ONNX Runtime session builder")?;
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

        let Io {
            input_spec,
            input_name,
            output_name,
            output_precision,
            output_dims,
            output_rows,
        } = read_io(&session)?;
        if let Err(error) = log_cpu_nodes(&session, prepare_id) {
            tracing::debug!(prepare_id, error = %format_args!("{error:#}"), "cpu_nodes_unavailable");
        }

        let allocator = Allocator::new(
            &session,
            MemoryInfo::new(
                AllocationDevice::DIRECTML,
                0,
                AllocatorType::Device,
                MemoryType::Default,
            )?,
        )?;
        let (input_tensor, input_resource) = DeviceTensor::create(
            &allocator,
            input_spec.precision,
            &[1, 3, input_spec.height, input_spec.width],
            "input",
            prepare_id,
        )?;
        // Must stay on the GPU: a captured graph replays with the same buffers.
        let (output_tensor, output_resource) = DeviceTensor::create(
            &allocator,
            output_precision,
            &output_dims,
            "output",
            prepare_id,
        )?;
        let readback_bytes = output_rows * OUTPUT_COLUMNS * output_precision.element_bytes();
        let readback = Readback::new(gpu, &output_resource, readback_bytes, low_latency)?;

        let mut binding = session.create_binding()?;
        input_tensor.bind_input(&mut binding, input_name)?;
        output_tensor.bind_output(&mut binding, output_name)?;

        let mut backend = Self {
            binding,
            _input_tensor: input_tensor,
            input_resource,
            readback,
            _allocator: allocator,
            session,
            _dml_device: dml_device,
            input_spec,
            prepare_id: prepare_id.to_owned(),
            output_precision,
            output_rows,
            output_values: Vec::with_capacity(output_rows * OUTPUT_COLUMNS),
            confidence_threshold: Confidence::default(),
            graph_capture,
        };
        backend.warm_up()?;
        Ok(backend)
    }

    fn warm_up(&mut self) -> Result<()> {
        let start = Instant::now();
        for _ in 0..WARM_UP_RUNS {
            self.session.run_binding(&self.binding)?;
            self.readback.read()?;
        }
        tracing::info!(
            prepare_id = %self.prepare_id,
            graph_capture = self.graph_capture,
            duration_ms = start.elapsed().as_secs_f64() * 1000.0,
            "inference_warm_up_complete"
        );
        Ok(())
    }

    pub(super) fn prepare_id(&self) -> &str {
        &self.prepare_id
    }

    pub(super) fn input_spec(&self) -> ModelInputSpec {
        self.input_spec
    }

    pub(super) fn input_resource(&self) -> &ID3D12Resource {
        &self.input_resource
    }

    pub(super) fn set_confidence_threshold(&mut self, value: Confidence) {
        self.confidence_threshold = value;
    }

    pub(super) fn run(&mut self) -> Result<InferenceOutput> {
        // The readback must queue behind the run on the same queue.
        self.session.run_binding(&self.binding)?;
        let bytes = self.readback.read()?;
        let inference_done_qpc = Clock::now();
        decode(bytes, self.output_precision, &mut self.output_values);
        let selection = select_nearest(
            &self.output_values,
            self.output_rows,
            self.input_spec.width,
            self.input_spec.height,
            self.confidence_threshold.ratio(),
        );
        Ok(InferenceOutput {
            selection,
            inference_done_qpc,
        })
    }
}

fn decode(bytes: &[u8], precision: Precision, values: &mut Vec<f32>) {
    values.clear();
    match precision {
        Precision::Fp32 => values.extend(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|value| f32::from_le_bytes(*value)),
        ),
        Precision::Fp16 => values.extend(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|value| half::f16::from_le_bytes(*value).to_f32()),
        ),
    }
}

fn select_nearest<T: Copy + Into<f32>>(
    data: &[T],
    rows: usize,
    input_width: usize,
    input_height: usize,
    confidence_threshold: f32,
) -> Selection {
    let image_center = (input_width as f32 * 0.5, input_height as f32 * 0.5);
    let mut best: Option<(f32, Detection)> = None;

    for row in data.as_chunks::<OUTPUT_COLUMNS>().0.iter().take(rows) {
        let confidence: f32 = row[4].into();
        if !confidence.is_finite() || confidence < confidence_threshold {
            continue;
        }
        let x1: f32 = row[0].into();
        let y1: f32 = row[1].into();
        let x2: f32 = row[2].into();
        let y2: f32 = row[3].into();
        let class_id: f32 = row[5].into();
        if !x1.is_finite()
            || !y1.is_finite()
            || !x2.is_finite()
            || !y2.is_finite()
            || x2 <= x1
            || y2 <= y1
        {
            continue;
        }

        let center_x = f32::midpoint(x1, x2);
        let center_y = f32::midpoint(y1, y2);
        let dx = center_x - image_center.0;
        let dy = center_y - image_center.1;
        let distance_sq = dx * dx + dy * dy;
        let detection = Detection {
            center_x,
            center_y,
            box_width: x2 - x1,
            box_height: y2 - y1,
            confidence,
            class_id: class_id.round() as i32,
        };
        if !matches!(best, Some((best_distance, _)) if best_distance <= distance_sq) {
            best = Some((distance_sq, detection));
        }
    }
    Selection {
        nearest: best.map(|(_, detection)| detection),
    }
}

struct Io {
    input_spec: ModelInputSpec,
    input_name: String,
    output_name: String,
    output_precision: Precision,
    output_dims: Vec<usize>,
    output_rows: usize,
}

fn read_io(session: &Session) -> Result<Io> {
    if session.inputs().len() != 1 || session.outputs().len() != 1 {
        bail!("expected exactly one model input and one model output");
    }
    let (input, output) = (&session.inputs()[0], &session.outputs()[0]);
    let dims: &[i64] = input
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
        precision: io_precision(input, "input")?,
    };
    let output_precision = io_precision(output, "output")?;
    let output_dims = validate_output_shape(output)?;
    let output_rows = output_dims[output_dims.len() - 2];
    validate_metadata(session, input_spec)?;
    Ok(Io {
        input_spec,
        input_name: input.name().to_owned(),
        output_name: output.name().to_owned(),
        output_precision,
        output_dims,
        output_rows,
    })
}

fn session_builder(low_latency: bool, graph_capture: bool) -> ort::Result<SessionBuilder> {
    let mut builder = Session::builder()?
        .with_memory_pattern(false)?
        .with_parallel_execution(false)?
        .with_intra_threads(1)?
        .with_intra_op_spinning(false)?
        .with_inter_op_spinning(false)?
        .with_config_entry("session.record_ep_graph_assignment_info", "1")?;
    if low_latency {
        builder = builder.with_config_entry("ep.dml.enable_cpu_sync_spinning", "1")?;
    }
    if graph_capture {
        // Needs the same bound buffers on each run, and no CPU node in the middle of the graph.
        builder = builder.with_config_entry("ep.dml.enable_graph_capture", "1")?;
    }
    Ok(builder)
}

fn validate_output_shape(outlet: &Outlet) -> Result<Vec<usize>> {
    let dims: &[i64] = outlet
        .dtype()
        .tensor_shape()
        .context("model output must be a tensor")?
        .as_ref();
    match dims {
        [1, rows, 6] | [rows, 6] if *rows > 0 => dims
            .iter()
            .map(|&size| usize::try_from(size).context("model output shape"))
            .collect(),
        _ => bail!(
            "expected a static end-to-end output [1, N, 6] (x1, y1, x2, y2, score, class), got {dims:?}; export the model with NMS embedded and a fixed input size"
        ),
    }
}

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

fn io_precision(outlet: &Outlet, role: &str) -> Result<Precision> {
    let element_type = outlet
        .dtype()
        .tensor_type()
        .with_context(|| format!("model {role} must be a tensor"))?;
    precision_of(element_type).with_context(|| {
        format!("unsupported model {role} element type {element_type}; expected FP16 or FP32")
    })
}

fn precision_of(element_type: TensorElementType) -> Option<Precision> {
    match element_type {
        TensorElementType::Float16 => Some(Precision::Fp16),
        TensorElementType::Float32 => Some(Precision::Fp32),
        _ => None,
    }
}

fn validate_tensor_resource(
    resource: &ID3D12Resource,
    expected_bytes: usize,
    role: &str,
) -> Result<()> {
    let desc = unsafe { resource.GetDesc() };
    if desc.Dimension != D3D12_RESOURCE_DIMENSION_BUFFER {
        bail!(
            "DirectML {role} resource must be a BUFFER, got {:?}",
            desc.Dimension
        );
    }
    if desc.Width < expected_bytes as u64 {
        bail!(
            "DirectML {role} resource is too small: expected at least {expected_bytes} bytes, got {}",
            desc.Width
        );
    }
    if desc.Flags.0 & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0 == 0 {
        bail!("DirectML {role} resource does not allow unordered access");
    }
    Ok(())
}

/// # Safety
/// `dml_device` and `queue` must belong to the same D3D12 device and outlive the resulting session.
unsafe fn append_dml1(
    builder: &mut SessionBuilder,
    dml_device: &IDMLDevice,
    queue: &ID3D12CommandQueue,
    prepare_id: &str,
) -> Result<()> {
    let api = ort::ep::directml::api().context("ONNX Runtime DirectML API is unavailable")?;
    // SAFETY: caller guarantees that these live COM interfaces are a matching DML device/queue pair.
    let status = unsafe {
        (api.SessionOptionsAppendExecutionProvider_DML1)(
            builder.ptr_mut().cast(),
            dml_device.as_raw(),
            queue.as_raw(),
        )
    };
    tracing::debug!(prepare_id, dml_device = ?dml_device.as_raw(), queue = ?queue.as_raw(), "dml1_registration_pointers");
    // SAFETY: status is returned by ONNX Runtime's DML1 API.
    unsafe { ort::Error::result_from_status(status) }.context("failed to append DirectML DML1")?;
    Ok(())
}

/// # Safety
/// `tensor` must be a live tensor allocated by this `DirectML` session's allocator.
unsafe fn d3d12_resource_from_dml_tensor<T: PrimitiveTensorElementType + Debug>(
    allocator: &Allocator,
    tensor: &mut Tensor<T>,
    prepare_id: &str,
) -> Result<ID3D12Resource> {
    let api = ort::ep::directml::api().context("ONNX Runtime DirectML API is unavailable")?;
    let mut raw = std::ptr::null_mut();
    tracing::debug!(prepare_id, allocator = ?allocator.ptr(), tensor_data = ?tensor.data_ptr_mut(), "dml_tensor_resource_pointers");
    // SAFETY: caller guarantees the DML tensor allocation and allocator are from the same session.
    let status = unsafe {
        (api.GetD3D12ResourceFromAllocation)(
            allocator.ptr().cast_mut(),
            tensor.data_ptr_mut().cast(),
            &mut raw,
        )
    };
    // SAFETY: status is returned by ONNX Runtime's DirectML API.
    unsafe { ort::Error::result_from_status(status) }
        .context("failed to get D3D12 resource for DirectML tensor")?;
    if raw.is_null() {
        bail!("DirectML returned a null D3D12 tensor resource");
    }
    // SAFETY: GetD3D12ResourceFromAllocation returns an AddRef'd ID3D12Resource per ORT contract.
    Ok(unsafe { ID3D12Resource::from_raw(raw.cast()) })
}

const CPU_PROVIDER: &str = "CPUExecutionProvider";

fn log_cpu_nodes(session: &Session, prepare_id: &str) -> Result<()> {
    let inner = session.inner();
    let mut subgraphs = std::ptr::null();
    let mut subgraph_count = 0_usize;
    let mut cpu_nodes = 0_usize;
    unsafe {
        ort_call((ort::api().Session_GetEpGraphAssignmentInfo)(
            inner.ptr(),
            &mut subgraphs,
            &mut subgraph_count,
        ))?;
        for &subgraph in raw_slice(subgraphs, subgraph_count, "EP subgraph list")? {
            if subgraph.is_null() {
                bail!("ORT returned a null EP subgraph");
            }
            if subgraph_provider_name(subgraph)? != CPU_PROVIDER {
                continue;
            }
            let mut nodes = std::ptr::null();
            let mut node_count = 0_usize;
            ort_call((ort::api().EpAssignedSubgraph_GetNodes)(
                subgraph,
                &mut nodes,
                &mut node_count,
            ))?;
            for &node in raw_slice(nodes, node_count, "EP node list")? {
                if node.is_null() {
                    bail!("ORT returned a null EP node");
                }
                let operator = node_text(
                    node,
                    ort::api().EpAssignedNode_GetOperatorType,
                    "operator type",
                )?;
                let domain = node_text(node, ort::api().EpAssignedNode_GetDomain, "domain")?;
                let name = node_text(node, ort::api().EpAssignedNode_GetName, "node name")?;
                tracing::warn!(prepare_id, %domain, %operator, %name, "cpu_fallback_node");
                cpu_nodes += 1;
            }
        }
    }
    tracing::info!(prepare_id, cpu_nodes, "cpu_fallback_nodes");
    Ok(())
}

type NodeTextFn = unsafe extern "system" fn(
    *const sys::OrtEpAssignedNode,
    *mut *const c_char,
) -> sys::OrtStatusPtr;

/// # Safety
/// `node` must be a live node of the session's assignment info, and `read` must be an ORT call
/// that writes a C string that the session owns.
unsafe fn node_text(
    node: *const sys::OrtEpAssignedNode,
    read: NodeTextFn,
    what: &str,
) -> Result<String> {
    let mut text = std::ptr::null();
    unsafe { ort_call(read(node, &mut text)) }?;
    if text.is_null() {
        bail!("ORT returned a null {what}");
    }
    Ok(unsafe { CStr::from_ptr(text) }
        .to_str()
        .with_context(|| format!("ORT returned a non-UTF-8 {what}"))?
        .to_owned())
}

/// # Safety
/// `subgraph` must be a live subgraph of the session's assignment info.
unsafe fn subgraph_provider_name(subgraph: *const sys::OrtEpAssignedSubgraph) -> Result<String> {
    let mut name = std::ptr::null();
    unsafe {
        ort_call((ort::api().EpAssignedSubgraph_GetEpName)(
            subgraph, &mut name,
        ))?;
    }
    if name.is_null() {
        bail!("ORT returned a null EP name");
    }
    Ok(unsafe { CStr::from_ptr(name) }
        .to_str()
        .context("ORT returned a non-UTF-8 EP name")?
        .to_owned())
}

/// # Safety
/// `status` must be returned by an ORT API call.
unsafe fn ort_call(status: sys::OrtStatusPtr) -> Result<()> {
    unsafe { ort::Error::result_from_status(status) }.map_err(anyhow::Error::from)
}

/// # Safety
/// `ptr` must point to `len` live elements, or `len` must be zero.
unsafe fn raw_slice<'a, T>(ptr: *const T, len: usize, description: &str) -> Result<&'a [T]> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        bail!("ORT returned a null {description} with length {len}");
    }
    Ok(unsafe { slice::from_raw_parts(ptr, len) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_valid_detection_wins() {
        let data: [f32; 12] = [
            0.0, 0.0, 20.0, 20.0, 0.95, 1.0, 45.0, 45.0, 55.0, 55.0, 0.80, 2.0,
        ];
        let detection = select_nearest(&data, 2, 100, 100, 0.25)
            .nearest
            .expect("detection");
        assert_eq!(detection.class_id, 2);
        assert_eq!((detection.center_x, detection.center_y), (50.0, 50.0));
        assert_eq!((detection.box_width, detection.box_height), (10.0, 10.0));
    }

    #[test]
    fn both_precisions_select_the_same_nearest_detection() {
        let data: [f32; 12] = [
            0.0, 0.0, 20.0, 20.0, 0.95, 1.0, 45.0, 45.0, 55.0, 55.0, 0.80, 2.0,
        ];
        let fp32 = select_nearest(&data, 2, 100, 100, 0.25)
            .nearest
            .expect("fp32 detection");
        let half_data = data.map(half::f16::from_f32);
        let fp16 = select_nearest(&half_data, 2, 100, 100, 0.25)
            .nearest
            .expect("fp16 detection");
        for detection in [fp32, fp16] {
            assert_eq!(detection.class_id, 2);
            assert_eq!((detection.center_x, detection.center_y), (50.0, 50.0));
        }
    }

    #[test]
    fn rows_below_the_threshold_or_with_a_non_finite_confidence_are_skipped() {
        let data: [f32; 12] = [
            0.0,
            0.0,
            20.0,
            20.0,
            0.10,
            1.0,
            45.0,
            45.0,
            55.0,
            55.0,
            f32::NAN,
            2.0,
        ];
        let selection = select_nearest(&data, 2, 100, 100, 0.25);
        assert!(selection.nearest.is_none());
    }

    #[test]
    fn a_row_with_non_finite_box_values_is_skipped_even_when_confident() {
        let data: [f32; 12] = [
            f32::INFINITY,
            0.0,
            20.0,
            20.0,
            0.95,
            1.0,
            45.0,
            45.0,
            55.0,
            55.0,
            0.80,
            2.0,
        ];
        let selection = select_nearest(&data, 2, 100, 100, 0.25);
        assert_eq!(selection.nearest.unwrap().class_id, 2);
    }

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
