use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use ort::{
    memory::{AllocationDevice, Allocator, AllocatorType, MemoryInfo, MemoryType},
    session::{IoBinding, Session},
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
        BackendInfo, BackendKind, BackendRunResult, Detection, InferenceBackend, ModelInputSpec,
        TensorPrecision, ep_assignment, postprocess::select_nearest,
    },
    settings::ConfidenceThreshold,
};

use super::{append_dml1, d3d12_resource_from_dml_tensor};

static NEXT_PREPARE_ID: AtomicU64 = AtomicU64::new(1);

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
        };
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

/// ONNX Runtime / DirectML session state. Device, queue, shared frames, and
/// preprocessing are owned by the enclosing InferencePipeline.
pub struct OrtDirectMlBackend {
    info: BackendInfo,
    input_spec: ModelInputSpec,
    prepare_id: String,
    binding: Option<IoBinding>,
    input_tensor: Option<InputTensor>,
    input_resource: Option<ID3D12Resource>,
    allocator: Option<Allocator>,
    session: Option<Session>,
    dml_device: Option<IDMLDevice>,
    output_name: String,
    output_precision: TensorPrecision,
    confidence_threshold: ConfidenceThreshold,
    ep_assignment: crate::inference::EpAssignmentState,
}

impl OrtDirectMlBackend {
    pub fn prepare(path: &Path, gpu: &GpuDeviceContext) -> Result<Self> {
        let prepare_id = format!(
            "{}-{}",
            std::process::id(),
            NEXT_PREPARE_ID.fetch_add(1, Ordering::Relaxed)
        );
        let total_start = Instant::now();
        tracing::info!(prepare_id = %prepare_id, model = %path.display(), "directml_prepare_begin");
        let result = Self::prepare_inner(path, gpu, &prepare_id);
        match result {
            Ok(backend) => {
                tracing::info!(prepare_id = %prepare_id, total_ms = total_start.elapsed().as_secs_f64() * 1000.0, "directml_prepare_complete");
                Ok(backend)
            }
            Err(error) => {
                tracing::error!(prepare_id = %prepare_id, error = %format!("{error:#}"), "directml_prepare_failed");
                Err(error)
            }
        }
    }

    fn prepare_inner(path: &Path, gpu: &GpuDeviceContext, prepare_id: &str) -> Result<Self> {
        if !path.is_file() {
            bail!("model does not exist: {}", path.display());
        }
        let mut dml_device = None;
        unsafe { DMLCreateDevice(gpu.device(), DML_CREATE_DEVICE_FLAG_NONE, &mut dml_device) }
            .context("failed to create IDMLDevice")?;
        let dml_device: IDMLDevice = dml_device.context("DMLCreateDevice returned no device")?;

        let mut builder = Session::builder()?
            .with_memory_pattern(false)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
            .with_parallel_execution(false)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
            .with_config_entry("session.record_ep_graph_assignment_info", "1")
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
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
            width: dims[3] as usize,
            height: dims[2] as usize,
            precision: io_precision(&session.inputs()[0], "input")?,
        };
        let output_precision = io_precision(&session.outputs()[0], "output")?;
        let input_name = session.inputs()[0].name().to_owned();
        let output_name = session.outputs()[0].name().to_owned();
        let ep_assignment = ep_assignment::read(&session)
            .map(crate::inference::EpAssignmentState::Available)
            .unwrap_or_else(|error| {
                crate::inference::EpAssignmentState::Unavailable(format!("{error:#}"))
            });

        // Declaration/drop order is protected by shutdown(), because tensors
        // must disappear before their DirectML allocator and session.
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
            info: BackendInfo {
                kind: BackendKind::OrtDirectMl,
                name: "ONNX Runtime DirectML".to_owned(),
                adapter_luid: gpu.adapter_luid(),
            },
            input_spec,
            prepare_id: prepare_id.to_owned(),
            binding: Some(binding),
            input_tensor: Some(input_tensor),
            input_resource: Some(input_resource),
            allocator: Some(allocator),
            session: Some(session),
            dml_device: Some(dml_device),
            output_name,
            output_precision,
            confidence_threshold: ConfidenceThreshold::default(),
            ep_assignment,
        })
    }

    pub fn prepare_id(&self) -> &str {
        &self.prepare_id
    }

    pub fn ep_assignment(&self) -> crate::inference::EpAssignmentState {
        self.ep_assignment.clone()
    }

    pub fn input_resource(&self) -> Result<&ID3D12Resource> {
        self.input_resource
            .as_ref()
            .context("DirectML input resource was shut down")
    }

    pub fn set_confidence_threshold(&mut self, value: ConfidenceThreshold) {
        self.confidence_threshold = value;
    }

    pub fn shutdown(&mut self) {
        self.binding.take();
        self.input_tensor.take();
        self.input_resource.take();
        self.allocator.take();
        self.session.take();
        self.dml_device.take();
    }
}

impl InferenceBackend for OrtDirectMlBackend {
    fn info(&self) -> &BackendInfo {
        &self.info
    }

    fn input_spec(&self) -> ModelInputSpec {
        self.input_spec
    }

    fn run(&mut self) -> Result<BackendRunResult> {
        // The tensor was bound once during prepare and is populated by the
        // pipeline's D3D12 preprocess dispatch before this call.
        let started = Instant::now();
        let outputs = self
            .session
            .as_mut()
            .context("DirectML session was shut down")?
            .run_binding(
                self.binding
                    .as_ref()
                    .context("DirectML IO binding was shut down")?,
            )?;
        let ort_run_wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        if outputs.len() != 1 {
            bail!("expected one model output, got {}", outputs.len());
        }
        let postprocess_start = Instant::now();
        let threshold = self.confidence_threshold.ratio();
        let output = &outputs[self.output_name.as_str()];
        let detection = match self.output_precision {
            TensorPrecision::Fp16 => detect::<half::f16>(output, self.input_spec, threshold)?,
            TensorPrecision::Fp32 => detect::<f32>(output, self.input_spec, threshold)?,
        };
        Ok(BackendRunResult {
            detection,
            ort_run_wall_ms,
            postprocess_ms: postprocess_start.elapsed().as_secs_f64() * 1000.0,
        })
    }

    fn set_confidence_threshold(&mut self, value: ConfidenceThreshold) {
        OrtDirectMlBackend::set_confidence_threshold(self, value);
    }

    fn shutdown(&mut self) {
        OrtDirectMlBackend::shutdown(self);
    }
}

fn detect<T: PrimitiveTensorElementType + Copy + Into<f32>>(
    output: &DynValue,
    spec: ModelInputSpec,
    confidence_threshold: f32,
) -> Result<Option<Detection>> {
    let (shape, data) = output.try_extract_tensor::<T>()?;
    let dims: &[i64] = shape.as_ref();
    let rows = match dims {
        [1, rows, 6] if *rows > 0 => *rows as usize,
        [rows, 6] if *rows > 0 => *rows as usize,
        _ => bail!("unsupported YOLO output shape {dims:?}; expected [1, N, 6]"),
    };
    if data.len() < rows.saturating_mul(6) {
        bail!("YOLO output buffer is shorter than its reported shape");
    }
    Ok(select_nearest(
        data,
        rows,
        spec.width,
        spec.height,
        confidence_threshold,
    ))
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
