//! The inference stage: the DirectML-backed ONNX Runtime session, its input tensor on the shared
//! D3D12 device, and the postprocess that turns the output into detections.

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

use super::{CpuNodes, Detection, GpuDeviceContext, Precision, readback::Readback};
use crate::{clock::Clock, settings::Confidence};

static NEXT_PREPARE_ID: AtomicU64 = AtomicU64::new(1);

/// The runs of the model that a load makes before the first frame. The first run of a graph capture
/// records the graph, and the others make sure that the replay works.
const WARM_UP_RUNS: usize = 3;

/// The values of a detection row: `[x1, y1, x2, y2, confidence, class_id]`.
const OUTPUT_COLUMNS: usize = 6;

/// The input tensor of the model: its width, height, and precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ModelInputSpec {
    pub width: usize,
    pub height: usize,
    pub precision: Precision,
}

/// The detections of one ONNX Runtime run, with the QPC ticks of its two stage boundaries.
pub(super) struct InferenceOutput {
    pub selection: Selection,
    /// After the ONNX Runtime run.
    pub inference_done_qpc: i64,
    /// After the postprocess.
    pub postprocess_done_qpc: i64,
}

/// The nearest detection that passed the confidence filter.
#[derive(Debug)]
pub(super) struct Selection {
    pub nearest: Option<Detection>,
}

/// A DirectML-allocated tensor, in the precision of the model.
enum DeviceTensor {
    Fp16(Tensor<half::f16>),
    Fp32(Tensor<f32>),
}

impl DeviceTensor {
    /// Allocates a tensor of `shape` and returns it with its backing D3D12 buffer. `role` names the
    /// tensor in the errors.
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

    /// The binding keeps the tensor for as long as it lives.
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

/// ONNX Runtime / `DirectML` session state. Device, queue, shared frames, and
/// preprocessing are owned by the enclosing pipeline.
///
/// Field order is drop order: tensors must disappear before their `DirectML`
/// allocator and session.
pub(super) struct OrtDirectMlBackend {
    binding: IoBinding,
    _input_tensor: DeviceTensor,
    input_resource: ID3D12Resource,
    /// Copies the output of the model, which stays on the GPU, to the CPU.
    readback: Readback,
    _allocator: Allocator,
    session: Session,
    _dml_device: IDMLDevice,
    input_spec: ModelInputSpec,
    prepare_id: String,
    output_precision: Precision,
    /// Rows of the static `[1, rows, 6]` or `[rows, 6]` output, validated at load.
    output_rows: usize,
    /// The output of the last run as `f32`, in the order of the model.
    output_values: Vec<f32>,
    confidence_threshold: Confidence,
    cpu_nodes: Option<CpuNodes>,
    /// ONNX Runtime records the graph of the model once and replays it on each run.
    graph_capture: bool,
}

impl Drop for OrtDirectMlBackend {
    fn drop(&mut self) {
        // The session may have work on the queue, such as the warm-up of a load that failed. It must be
        // done before the fields below are released.
        if let Err(error) = self.readback.drain() {
            tracing::warn!(prepare_id = %self.prepare_id, error = %format_args!("{error:#}"), "inference_backend_drain_failed");
        }
    }
}

impl OrtDirectMlBackend {
    /// Loads the model with the graph capture of `DirectML`. A model that cannot use it loads without it.
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
        // A failed read leaves the count unknown. The log keeps the reason.
        let cpu_nodes = read_cpu_nodes(&session, prepare_id)
            .inspect_err(|error| {
                tracing::debug!(prepare_id, error = %format_args!("{error:#}"), "cpu_nodes_unavailable");
            })
            .ok();

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
        // The output stays on the GPU: a captured graph replays with the same buffers on each run.
        let (output_tensor, output_resource) = DeviceTensor::create(
            &allocator,
            output_precision,
            &output_dims,
            "output",
            prepare_id,
        )?;
        // Only the rows are read back, and not the padding of the last word.
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
            cpu_nodes,
            graph_capture,
        };
        backend.warm_up()?;
        Ok(backend)
    }

    /// Runs the model a few times on the input as it is, before the first frame. The first run of a
    /// graph capture records the graph, and the next runs replay it. A failure fails the load.
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

    /// Whether ONNX Runtime replays a captured graph on each run.
    pub(super) fn graph_capture(&self) -> bool {
        self.graph_capture
    }

    pub(super) fn prepare_id(&self) -> &str {
        &self.prepare_id
    }

    pub(super) fn input_spec(&self) -> ModelInputSpec {
        self.input_spec
    }

    /// The nodes that ORT assigned to the CPU. `None` when ORT does not report the assignment.
    pub(super) fn cpu_nodes(&self) -> Option<&CpuNodes> {
        self.cpu_nodes.as_ref()
    }

    pub(super) fn input_resource(&self) -> &ID3D12Resource {
        &self.input_resource
    }

    pub(super) fn set_confidence_threshold(&mut self, value: Confidence) {
        self.confidence_threshold = value;
    }

    /// Runs the model on the input that the preprocess of the pipeline filled, and waits for the output.
    /// The tensors were bound once in `prepare`.
    pub(super) fn run(&mut self) -> Result<InferenceOutput> {
        // The run only queues the work on the GPU. The readback goes behind it on the same queue.
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
            postprocess_done_qpc: Clock::now(),
        })
    }
}

/// Turns the little-endian values of the output into `f32`. The buffer is reused.
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

/// `data` holds `rows` YOLO rows of `[x1, y1, x2, y2, confidence, class_id]`
/// in either FP32 or FP16.
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
        // The confidence decides the row first, so a rejected row converts no box values.
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

/// What the session says about its one input and its one output. Everything here is validated.
struct Io {
    input_spec: ModelInputSpec,
    input_name: String,
    output_name: String,
    output_precision: Precision,
    /// `[1, rows, 6]` or `[rows, 6]`.
    output_dims: Vec<usize>,
    output_rows: usize,
}

/// Checks that the model has one static input `[1, 3, H, W]` and one end-to-end output, and reads them.
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
    // The shape ends in `[rows, 6]`.
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
    if graph_capture {
        // DirectML records the commands of the graph on the first run and replays them on the next
        // runs, so a run costs no CPU time to build the commands. It needs the same bound buffers on
        // each run, and no node that moves data to the CPU in the middle of the graph. A model that
        // does not fit fails to load here, and the caller loads it again without this.
        builder = builder.with_config_entry("ep.dml.enable_graph_capture", "1")?;
    }
    Ok(builder)
}

/// The output must be a static end-to-end `[1, N, 6]` (or `[N, 6]`) detection
/// list; returns its shape.
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

/// Checks the buffer of a `DirectML` tensor. `role` is `input` or `output`.
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

// ---- The ORT C API, for the parts that the safe wrapper does not expose ----

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

/// Reads the nodes that ORT assigned to the CPU provider, by operator type. Each node is also logged
/// with its name, so that the node can be found in the model.
fn read_cpu_nodes(session: &Session, prepare_id: &str) -> Result<CpuNodes> {
    let inner = session.inner();
    let mut subgraphs = std::ptr::null();
    let mut subgraph_count = 0_usize;
    let mut operators = Vec::new();
    // ORT owns the assignment records for the lifetime of the session. Nothing is retained here.
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
                tracing::info!(prepare_id, %domain, %operator, %name, "cpu_fallback_node");
                operators.push(operator_label(&domain, &operator));
            }
        }
    }
    Ok(CpuNodes::from_operators(operators))
}

/// The operator type as the panel shows it. The default ONNX domain is left out.
fn operator_label(domain: &str, operator: &str) -> String {
    match domain {
        "" | "ai.onnx" => operator.to_owned(),
        domain => format!("{domain}::{operator}"),
    }
}

/// An ORT call that reads one text of an assigned node.
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
    fn the_default_onnx_domain_is_left_out_of_the_operator_label() {
        assert_eq!(operator_label("", "Gather"), "Gather");
        assert_eq!(operator_label("ai.onnx", "Gather"), "Gather");
        assert_eq!(
            operator_label("com.microsoft", "FusedConv"),
            "com.microsoft::FusedConv"
        );
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

    // ---- GPU tests ----

    use std::path::PathBuf;

    use windows::Win32::Graphics::{
        Direct3D12::{
            D3D12_COMMAND_LIST_TYPE_COMPUTE, D3D12_CPU_PAGE_PROPERTY_UNKNOWN, D3D12_HEAP_FLAG_NONE,
            D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_UPLOAD, D3D12_MEMORY_POOL_UNKNOWN,
            D3D12_RESOURCE_DESC, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COPY_DEST,
            D3D12_RESOURCE_STATE_GENERIC_READ, D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            D3D12_TEXTURE_LAYOUT_ROW_MAJOR, ID3D12CommandAllocator, ID3D12CommandList,
            ID3D12GraphicsCommandList, ID3D12PipelineState,
        },
        Dxgi::Common::{DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC},
    };

    use crate::pipeline::{displays, gpu::transition, init_thread};

    /// The model of the GPU tests is the file that `NICE_AIMASSIST_TEST_MODEL` names. Without the
    /// variable the test has no model and skips itself, so that `cargo test -- --ignored` still passes.
    fn test_model() -> Option<PathBuf> {
        let model = std::env::var_os("NICE_AIMASSIST_TEST_MODEL").map(PathBuf::from);
        if model.is_none() {
            println!("NICE_AIMASSIST_TEST_MODEL is not set: the test is skipped");
        }
        model
    }

    /// Writes a pseudo-random image in `[0, 1)` into the input tensor. Another `seed` is another image.
    fn fill_input(
        gpu: &mut GpuDeviceContext,
        backend: &OrtDirectMlBackend,
        seed: u32,
    ) -> Result<()> {
        let spec = backend.input_spec();
        let elements = spec.width * spec.height * 3;
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(88_172_645);
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 8) as f32 / (1_u32 << 24) as f32
        };
        let bytes: Vec<u8> = match spec.precision {
            Precision::Fp32 => (0..elements).flat_map(|_| next().to_le_bytes()).collect(),
            Precision::Fp16 => (0..elements)
                .flat_map(|_| half::f16::from_f32(next()).to_le_bytes())
                .collect(),
        };

        let device = gpu.device().clone();
        let properties = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_UPLOAD,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0,
            Width: bytes.len() as u64,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_UNKNOWN,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: D3D12_RESOURCE_FLAG_NONE,
        };
        let mut upload = None;
        unsafe {
            device.CreateCommittedResource(
                &properties,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_GENERIC_READ,
                None,
                &mut upload,
            )
        }?;
        let upload: ID3D12Resource = upload.context("no upload buffer")?;
        let mut mapped = std::ptr::null_mut();
        unsafe { upload.Map(0, None, Some(&mut mapped)) }?;
        // SAFETY: the buffer holds `bytes.len()` bytes, and it is mapped for writing.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast::<u8>(), bytes.len()) };
        unsafe { upload.Unmap(0, None) };

        let allocator: ID3D12CommandAllocator =
            unsafe { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COMPUTE) }?;
        let list: ID3D12GraphicsCommandList = unsafe {
            device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_COMPUTE,
                &allocator,
                None::<&ID3D12PipelineState>,
            )
        }?;
        let input = backend.input_resource();
        unsafe {
            list.ResourceBarrier(&[transition(
                input,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COPY_DEST,
            )]);
            list.CopyBufferRegion(input, 0, &upload, 0, bytes.len() as u64);
            list.ResourceBarrier(&[transition(
                input,
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            )]);
            list.Close()
        }?;
        let list: ID3D12CommandList = list.cast()?;
        unsafe { gpu.queue().ExecuteCommandLists(&[Some(list)]) };
        gpu.wait_idle()
    }

    /// The mean of a series of times, in milliseconds, and its 95th percentile.
    fn mean_and_p95(mut times: Vec<f64>) -> (f64, f64) {
        times.sort_by(f64::total_cmp);
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        (mean, times[times.len() * 95 / 100])
    }

    /// The time of a run, and the part of it that the CPU spends to queue the work, in milliseconds.
    /// Each is `(mean, p95)`.
    fn time_runs(
        gpu: &mut GpuDeviceContext,
        backend: &mut OrtDirectMlBackend,
        runs: usize,
    ) -> Result<((f64, f64), (f64, f64))> {
        fill_input(gpu, backend, 7)?;
        let mut totals = Vec::with_capacity(runs);
        let mut queueing = Vec::with_capacity(runs);
        for _ in 0..runs {
            let start = Instant::now();
            backend.session.run_binding(&backend.binding)?;
            queueing.push(start.elapsed().as_secs_f64() * 1000.0);
            backend.readback.read()?;
            totals.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        Ok((mean_and_p95(totals), mean_and_p95(queueing)))
    }

    #[test]
    #[ignore = "needs a GPU and the model that NICE_AIMASSIST_TEST_MODEL names"]
    fn a_replayed_graph_gives_the_same_output_as_a_plain_run() -> Result<()> {
        let Some(model) = test_model() else {
            return Ok(());
        };
        init_thread()?;
        let display = displays()?
            .into_iter()
            .next()
            .context("no display is available")?;
        let mut plain_gpu = GpuDeviceContext::new(display.adapter_luid)?;
        let mut captured_gpu = GpuDeviceContext::new(display.adapter_luid)?;
        let mut plain =
            OrtDirectMlBackend::prepare_inner(&model, &plain_gpu, "plain", false, false)?;
        let mut captured =
            OrtDirectMlBackend::prepare_inner(&model, &captured_gpu, "captured", false, true)?;
        assert!(captured.graph_capture() && !plain.graph_capture());

        let mut first_outputs: Vec<Vec<f32>> = Vec::new();
        for step in 0..12_u32 {
            // Three images in turn. A replay that kept the first image would differ from the plain run.
            let seed = step % 3;
            fill_input(&mut plain_gpu, &plain, seed)?;
            fill_input(&mut captured_gpu, &captured, seed)?;
            plain.run()?;
            captured.run()?;
            assert_eq!(plain.output_values.len(), captured.output_values.len());
            for (index, (a, b)) in plain
                .output_values
                .iter()
                .zip(&captured.output_values)
                .enumerate()
            {
                assert!(
                    (a - b).abs() <= 1e-3 * a.abs().max(1.0),
                    "step {step}, value {index}: plain {a}, captured {b}"
                );
            }
            if first_outputs.len() < 3 {
                first_outputs.push(plain.output_values.clone());
            } else {
                // The same image gives the same output again.
                assert_eq!(first_outputs[seed as usize], plain.output_values);
            }
        }
        assert_ne!(
            first_outputs[0], first_outputs[1],
            "two different images must give two different outputs"
        );
        Ok(())
    }

    #[test]
    #[ignore = "needs a GPU and the model that NICE_AIMASSIST_TEST_MODEL names"]
    fn a_model_loads_and_runs_with_or_without_graph_capture() -> Result<()> {
        let Some(model) = test_model() else {
            return Ok(());
        };
        init_thread()?;
        let display = displays()?
            .into_iter()
            .next()
            .context("no display is available")?;
        let mut gpu = GpuDeviceContext::new(display.adapter_luid)?;
        // A model with a node on the CPU cannot capture its graph, and `prepare` then loads it without.
        let mut backend = OrtDirectMlBackend::prepare(&model, &gpu, false)?;
        fill_input(&mut gpu, &backend, 3)?;
        for _ in 0..5 {
            backend.run()?;
        }
        assert_eq!(
            backend.output_values.len(),
            backend.output_rows * OUTPUT_COLUMNS
        );
        println!(
            "graph_capture={} cpu_nodes={:?}",
            backend.graph_capture(),
            backend.cpu_nodes().map(CpuNodes::summary)
        );
        Ok(())
    }

    #[test]
    #[ignore = "needs a GPU and the model that NICE_AIMASSIST_TEST_MODEL names"]
    fn timing_of_a_run_with_and_without_graph_capture() -> Result<()> {
        let Some(model) = test_model() else {
            return Ok(());
        };
        init_thread()?;
        let display = displays()?
            .into_iter()
            .next()
            .context("no display is available")?;
        for low_latency in [false, true] {
            for graph_capture in [false, true] {
                let mut gpu = GpuDeviceContext::new(display.adapter_luid)?;
                let mut backend = OrtDirectMlBackend::prepare_inner(
                    &model,
                    &gpu,
                    "timing",
                    low_latency,
                    graph_capture,
                )?;
                let ((mean, p95), (queue_mean, queue_p95)) =
                    time_runs(&mut gpu, &mut backend, 300)?;
                println!(
                    "low_latency={low_latency} graph_capture={graph_capture}: run {mean:.3} ms (p95 {p95:.3}), CPU queueing {queue_mean:.3} ms (p95 {queue_p95:.3})"
                );
            }
        }
        Ok(())
    }
}
