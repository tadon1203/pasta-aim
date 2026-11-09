use std::mem::{ManuallyDrop, size_of};

use super::gpu::{GpuDeviceContext, transition};
use crate::{
    clock::Clock,
    pipeline::{GpuFaultStage, Precision},
};
use anyhow::{Context, Result, bail};
use windows::{
    Win32::Graphics::{
        Direct3D::Fxc::D3DCompile,
        Direct3D::ID3DBlob,
        Direct3D12::{
            D3D_ROOT_SIGNATURE_VERSION_1, D3D12_BUFFER_UAV, D3D12_BUFFER_UAV_FLAG_NONE,
            D3D12_CACHED_PIPELINE_STATE, D3D12_COMMAND_LIST_TYPE_COMPUTE,
            D3D12_COMPUTE_PIPELINE_STATE_DESC, D3D12_CPU_DESCRIPTOR_HANDLE,
            D3D12_CPU_PAGE_PROPERTY_UNKNOWN, D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            D3D12_DESCRIPTOR_HEAP_DESC, D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
            D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, D3D12_DESCRIPTOR_RANGE,
            D3D12_DESCRIPTOR_RANGE_OFFSET_APPEND, D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
            D3D12_DESCRIPTOR_RANGE_TYPE_UAV, D3D12_FEATURE_DATA_FORMAT_SUPPORT,
            D3D12_FEATURE_FORMAT_SUPPORT, D3D12_FORMAT_SUPPORT2_UAV_TYPED_STORE,
            D3D12_GPU_DESCRIPTOR_HANDLE, D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES,
            D3D12_HEAP_TYPE_READBACK, D3D12_MEMORY_POOL_UNKNOWN, D3D12_PIPELINE_STATE_FLAG_NONE,
            D3D12_QUERY_HEAP_DESC, D3D12_QUERY_HEAP_TYPE_TIMESTAMP, D3D12_QUERY_TYPE_TIMESTAMP,
            D3D12_RANGE, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0,
            D3D12_RESOURCE_BARRIER_FLAG_NONE, D3D12_RESOURCE_BARRIER_TYPE_UAV, D3D12_RESOURCE_DESC,
            D3D12_RESOURCE_DIMENSION_BUFFER, D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON,
            D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            D3D12_RESOURCE_UAV_BARRIER, D3D12_ROOT_CONSTANTS, D3D12_ROOT_DESCRIPTOR_TABLE,
            D3D12_ROOT_PARAMETER, D3D12_ROOT_PARAMETER_0,
            D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS, D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            D3D12_ROOT_SIGNATURE_DESC, D3D12_ROOT_SIGNATURE_FLAG_NONE, D3D12_SHADER_BYTECODE,
            D3D12_SHADER_RESOURCE_VIEW_DESC, D3D12_SHADER_RESOURCE_VIEW_DESC_0,
            D3D12_SHADER_VISIBILITY_ALL, D3D12_SRV_DIMENSION_TEXTURE2D, D3D12_TEX2D_SRV,
            D3D12_TEXTURE_LAYOUT_ROW_MAJOR, D3D12_UAV_DIMENSION_BUFFER,
            D3D12_UNORDERED_ACCESS_VIEW_DESC, D3D12_UNORDERED_ACCESS_VIEW_DESC_0,
            D3D12SerializeRootSignature, ID3D12CommandAllocator, ID3D12CommandList,
            ID3D12CommandQueue, ID3D12DescriptorHeap, ID3D12Device, ID3D12Fence,
            ID3D12GraphicsCommandList, ID3D12PipelineState, ID3D12QueryHeap, ID3D12Resource,
            ID3D12RootSignature,
        },
        Dxgi::Common::{
            DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16_FLOAT, DXGI_FORMAT_R32_FLOAT,
        },
    },
    core::{Interface, PCSTR},
};

const SHADER: &str = r"
Texture2D<float4> source_image : register(t0);
RWBuffer<float> destination : register(u0);
cbuffer Params : register(b0) { uint width; uint height; uint pixels; };
[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (id.x >= width || id.y >= height) return;
    uint pixel = id.y * width + id.x;
    float4 color = source_image.Load(int3(id.x, id.y, 0));
    destination[pixel] = color.r;
    destination[pixels + pixel] = color.g;
    destination[2 * pixels + pixel] = color.b;
}
";
const COLOR_CHANNELS: usize = 3;
const PREPROCESS_PARAMETER_COUNT: u32 = 3;
const THREADS_PER_GROUP: usize = 8;
const MAX_DISPATCH_GROUPS: usize = 65_535;

impl std::fmt::Debug for GpuPreprocessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuPreprocessor").finish_non_exhaustive()
    }
}

/// Field order is drop order: the recorded lists go before everything they refer to.
pub struct GpuPreprocessor {
    timed_list: ID3D12CommandList,
    untimed_list: ID3D12CommandList,
    _allocator: ID3D12CommandAllocator,
    _descriptor_heap: ID3D12DescriptorHeap,
    _pipeline: ID3D12PipelineState,
    _root_signature: ID3D12RootSignature,
    _destination: ID3D12Resource,
    _source: ID3D12Resource,
    queue: ID3D12CommandQueue,
    list_in_flight: Option<u64>,
    timestamps: PreprocessTimestamps,
    fault_stage: GpuFaultStage,
}

#[derive(Debug, Clone, Copy)]
pub struct PreprocessTicket {
    pub completion_value: u64,
    pub frame_seq: u64,
    pub timed: bool,
}

struct PreprocessTimestamps {
    heap: ID3D12QueryHeap,
    readback: ID3D12Resource,
    mapped: *const u64,
    frequency: u64,
    pending: Option<u64>,
}

impl GpuPreprocessor {
    pub fn new(
        gpu: &GpuDeviceContext,
        source: &ID3D12Resource,
        destination: &ID3D12Resource,
        width: usize,
        height: usize,
        precision: Precision,
    ) -> Result<Self> {
        let device = gpu.device();
        if width == 0 || height == 0 || width > u32::MAX as usize || height > u32::MAX as usize {
            bail!("invalid GPU preprocess dimensions {width}x{height}");
        }
        let pixels = width.checked_mul(height).context("input size overflowed")?;
        let elements = pixels
            .checked_mul(COLOR_CHANNELS)
            .context("input tensor size overflowed")?;
        let dispatch_x = width.div_ceil(THREADS_PER_GROUP);
        let dispatch_y = height.div_ceil(THREADS_PER_GROUP);
        if elements > u32::MAX as usize
            || dispatch_x > MAX_DISPATCH_GROUPS
            || dispatch_y > MAX_DISPATCH_GROUPS
        {
            bail!("input {width}x{height} is too large for the GPU preprocess dispatch");
        }
        let format = tensor_format(precision);
        ensure_typed_uav_store(device, format, precision)?;

        let (root_signature, pipeline) = create_pipeline(device)?;
        let heap_desc = D3D12_DESCRIPTOR_HEAP_DESC {
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
            NumDescriptors: 2,
            Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
            NodeMask: 0,
        };
        let descriptor_heap: ID3D12DescriptorHeap =
            unsafe { device.CreateDescriptorHeap(&heap_desc) }
                .context("failed to create preprocess descriptor heap")?;
        let descriptor_stride = unsafe {
            device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV)
        };
        let allocator: ID3D12CommandAllocator =
            unsafe { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COMPUTE) }
                .context("failed to create preprocess command allocator")?;
        let frequency = unsafe { gpu.queue().GetTimestampFrequency() }
            .context("failed to query compute timestamp frequency")?;
        let timestamps = PreprocessTimestamps::new(device, frequency)?;

        let cpu_start = unsafe { descriptor_heap.GetCPUDescriptorHandleForHeapStart() };
        write_descriptors(
            device,
            source,
            destination,
            format,
            elements,
            cpu_start,
            descriptor_stride,
        );

        let gpu_base = unsafe { descriptor_heap.GetGPUDescriptorHandleForHeapStart() };
        let output_gpu = offset_gpu(gpu_base, descriptor_stride);
        let constants = [width as u32, height as u32, pixels as u32];
        let record = |timed: bool| -> Result<ID3D12CommandList> {
            let list = PreprocessRecording {
                device,
                allocator: &allocator,
                pipeline: &pipeline,
                root_signature: &root_signature,
                descriptor_heap: &descriptor_heap,
                source,
                destination,
                source_gpu: gpu_base,
                output_gpu,
                timestamps: &timestamps,
                constants,
                dispatch: (dispatch_x as u32, dispatch_y as u32),
                timed,
            }
            .record()?;
            list.cast()
                .context("failed to cast the preprocess command list")
        };
        let timed_list = record(true)?;
        let untimed_list = record(false)?;

        Ok(Self {
            timed_list,
            untimed_list,
            _allocator: allocator,
            _descriptor_heap: descriptor_heap,
            _pipeline: pipeline,
            _root_signature: root_signature,
            _destination: destination.clone(),
            _source: source.clone(),
            queue: gpu.queue().clone(),
            list_in_flight: None,
            timestamps,
            fault_stage: GpuFaultStage::PreprocessSubmit,
        })
    }

    pub fn submit(
        &mut self,
        frame_seq: u64,
        ready_fence: &ID3D12Fence,
        reuse_fence: &ID3D12Fence,
        reuse_completed: u64,
        measure: bool,
    ) -> Result<PreprocessTicket> {
        self.fault_stage = GpuFaultStage::PreprocessSubmit;
        if super::gpu::fence_reports_removal(reuse_completed) {
            self.fault_stage = GpuFaultStage::DeviceRemoved;
            bail!("D3D12 reuse fence reports device removed");
        }
        if self
            .list_in_flight
            .is_some_and(|value| reuse_completed < value)
        {
            bail!("preprocess command list reused before its reuse fence completed");
        }
        self.fault_stage = GpuFaultStage::D3D12ReadyWait;
        if let Err(error) = unsafe { self.queue.Wait(ready_fence, frame_seq) } {
            return Err(error).context("D3D12 queue wait on capture fence failed");
        }
        self.fault_stage = GpuFaultStage::PreprocessSubmit;
        if measure {
            self.timestamps.pending = Some(frame_seq);
        }
        let list = if measure {
            &self.timed_list
        } else {
            &self.untimed_list
        };
        unsafe { self.queue.ExecuteCommandLists(&[Some(list.clone())]) };
        self.fault_stage = GpuFaultStage::ReuseSignal;
        if let Err(error) = unsafe { self.queue.Signal(reuse_fence, frame_seq) } {
            return Err(error).context("D3D12 queue signal on reuse fence failed");
        }
        self.list_in_flight = Some(frame_seq);
        Ok(PreprocessTicket {
            completion_value: frame_seq,
            frame_seq,
            timed: measure,
        })
    }

    pub fn collect(
        &mut self,
        ticket: PreprocessTicket,
        reuse_fence: &ID3D12Fence,
    ) -> Result<Option<f64>> {
        if !ticket.timed {
            return Ok(None);
        }
        let completed = unsafe { reuse_fence.GetCompletedValue() };
        if super::gpu::fence_reports_removal(completed) {
            self.fault_stage = GpuFaultStage::DeviceRemoved;
            bail!("D3D12 reuse fence reports device removed while collecting preprocess timestamp");
        }
        if completed < ticket.completion_value {
            bail!(
                "preprocess timestamp contract violation: frame_seq={} required_fence={} completed_fence={completed}",
                ticket.frame_seq,
                ticket.completion_value
            );
        }
        self.timestamps.collect(ticket.completion_value).map(Some)
    }

    pub fn fault_stage(&self) -> GpuFaultStage {
        self.fault_stage
    }
}

fn tensor_format(precision: Precision) -> DXGI_FORMAT {
    match precision {
        Precision::Fp32 => DXGI_FORMAT_R32_FLOAT,
        Precision::Fp16 => DXGI_FORMAT_R16_FLOAT,
    }
}

fn ensure_typed_uav_store(
    device: &ID3D12Device,
    format: DXGI_FORMAT,
    precision: Precision,
) -> Result<()> {
    let mut support = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
        Format: format,
        ..Default::default()
    };
    unsafe {
        device.CheckFeatureSupport(
            D3D12_FEATURE_FORMAT_SUPPORT,
            std::ptr::from_mut::<D3D12_FEATURE_DATA_FORMAT_SUPPORT>(&mut support).cast(),
            size_of::<D3D12_FEATURE_DATA_FORMAT_SUPPORT>() as u32,
        )
    }
    .context("failed to query the format support of the GPU")?;
    if support.Support2.0 & D3D12_FORMAT_SUPPORT2_UAV_TYPED_STORE.0 == 0 {
        bail!(
            "this display's GPU cannot store {} model input through typed unordered access; the pipeline cannot be built",
            precision.label()
        );
    }
    Ok(())
}

struct PreprocessRecording<'a> {
    device: &'a ID3D12Device,
    allocator: &'a ID3D12CommandAllocator,
    pipeline: &'a ID3D12PipelineState,
    root_signature: &'a ID3D12RootSignature,
    descriptor_heap: &'a ID3D12DescriptorHeap,
    source: &'a ID3D12Resource,
    destination: &'a ID3D12Resource,
    source_gpu: D3D12_GPU_DESCRIPTOR_HANDLE,
    output_gpu: D3D12_GPU_DESCRIPTOR_HANDLE,
    timestamps: &'a PreprocessTimestamps,
    constants: [u32; PREPROCESS_PARAMETER_COUNT as usize],
    dispatch: (u32, u32),
    timed: bool,
}

impl PreprocessRecording<'_> {
    fn record(&self) -> Result<ID3D12GraphicsCommandList> {
        let list: ID3D12GraphicsCommandList = unsafe {
            self.device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_COMPUTE,
                self.allocator,
                self.pipeline,
            )
        }
        .context("failed to create preprocess command list")?;
        let first_query = 0;
        unsafe {
            list.ResourceBarrier(&[transition(
                self.source,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            )]);
            list.SetComputeRootSignature(self.root_signature);
            list.SetPipelineState(self.pipeline);
            list.SetDescriptorHeaps(&[Some(self.descriptor_heap.clone())]);
            list.SetComputeRootDescriptorTable(0, self.source_gpu);
            list.SetComputeRootDescriptorTable(1, self.output_gpu);
            list.SetComputeRoot32BitConstants(
                2,
                PREPROCESS_PARAMETER_COUNT,
                self.constants.as_ptr().cast(),
                0,
            );
            if self.timed {
                list.EndQuery(
                    &self.timestamps.heap,
                    D3D12_QUERY_TYPE_TIMESTAMP,
                    first_query,
                );
            }
            list.Dispatch(self.dispatch.0, self.dispatch.1, 1);
            if self.timed {
                list.EndQuery(
                    &self.timestamps.heap,
                    D3D12_QUERY_TYPE_TIMESTAMP,
                    first_query + 1,
                );
                list.ResolveQueryData(
                    &self.timestamps.heap,
                    D3D12_QUERY_TYPE_TIMESTAMP,
                    first_query,
                    2,
                    &self.timestamps.readback,
                    0,
                );
            }
            list.ResourceBarrier(&[D3D12_RESOURCE_BARRIER {
                Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
                Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                    UAV: ManuallyDrop::new(D3D12_RESOURCE_UAV_BARRIER {
                        pResource: ManuallyDrop::new(Some(self.destination.clone())),
                    }),
                },
            }]);
            list.ResourceBarrier(&[transition(
                self.source,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            )]);
        }
        unsafe { list.Close() }.context("failed to close preprocess command list")?;
        Ok(list)
    }
}

impl PreprocessTimestamps {
    fn new(device: &ID3D12Device, frequency: u64) -> Result<Self> {
        let desc = D3D12_QUERY_HEAP_DESC {
            Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
            Count: 2,
            NodeMask: 0,
        };
        let mut heap = None;
        unsafe { device.CreateQueryHeap(&desc, &mut heap) }
            .context("failed to create preprocess timestamp query heap")?;
        let heap = heap.context("timestamp query heap creation returned no heap")?;
        let properties = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_READBACK,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        };
        let resource_desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0,
            Width: (2 * std::mem::size_of::<u64>()) as u64,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: D3D12_RESOURCE_FLAG_NONE,
        };
        let mut readback = None;
        unsafe {
            device.CreateCommittedResource(
                &properties,
                D3D12_HEAP_FLAG_NONE,
                &resource_desc,
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut readback,
            )
        }
        .context("failed to create timestamp readback buffer")?;
        let readback: ID3D12Resource =
            readback.context("timestamp readback creation returned no resource")?;
        let range = D3D12_RANGE {
            Begin: 0,
            End: 2 * std::mem::size_of::<u64>(),
        };
        let mut mapped = std::ptr::null_mut();
        unsafe { readback.Map(0, Some(&range), Some(&mut mapped)) }
            .context("failed to map timestamp readback buffer")?;
        Ok(Self {
            heap,
            readback,
            mapped: mapped.cast(),
            frequency,
            pending: None,
        })
    }

    fn collect(&mut self, completion_value: u64) -> Result<f64> {
        if self.pending != Some(completion_value) {
            bail!("preprocess timestamp ticket does not match its submission");
        }
        // The same compute queue has completed this fence value, so timestamp
        // readback is ready without a CPU wait.
        let values = unsafe { std::slice::from_raw_parts(self.mapped, 2) };
        if values[1] < values[0] || self.frequency == 0 {
            bail!("invalid preprocess timestamp pair");
        }
        self.pending = None;
        Ok(Clock::with_frequency(self.frequency)
            .ms(values[0].cast_signed(), values[1].cast_signed()))
    }
}

impl Drop for PreprocessTimestamps {
    fn drop(&mut self) {
        let empty = D3D12_RANGE { Begin: 0, End: 0 };
        unsafe { self.readback.Unmap(0, Some(&empty)) };
    }
}

fn create_root_signature(device: &ID3D12Device) -> Result<ID3D12RootSignature> {
    let ranges = [
        D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
            NumDescriptors: 1,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: D3D12_DESCRIPTOR_RANGE_OFFSET_APPEND,
        },
        D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
            NumDescriptors: 1,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: D3D12_DESCRIPTOR_RANGE_OFFSET_APPEND,
        },
    ];
    let parameters = [
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: &ranges[0],
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: &ranges[1],
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Constants: D3D12_ROOT_CONSTANTS {
                    ShaderRegister: 0,
                    RegisterSpace: 0,
                    Num32BitValues: PREPROCESS_PARAMETER_COUNT,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        },
    ];
    let root_desc = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: parameters.len() as u32,
        pParameters: parameters.as_ptr(),
        NumStaticSamplers: 0,
        pStaticSamplers: std::ptr::null(),
        Flags: D3D12_ROOT_SIGNATURE_FLAG_NONE,
    };
    let mut serialized: Option<ID3DBlob> = None;
    unsafe {
        D3D12SerializeRootSignature(
            &root_desc,
            D3D_ROOT_SIGNATURE_VERSION_1,
            &mut serialized,
            None,
        )
    }
    .context("failed to serialize preprocess root signature")?;
    let serialized = serialized.context("root signature serialization returned no blob")?;
    let signature_bytes = unsafe {
        std::slice::from_raw_parts(
            serialized.GetBufferPointer().cast::<u8>(),
            serialized.GetBufferSize(),
        )
    };
    unsafe { device.CreateRootSignature(0, signature_bytes) }
        .context("failed to create preprocess root signature")
}

fn compile_shader() -> Result<ID3DBlob> {
    let source = SHADER.as_bytes();
    let entry = PCSTR(c"main".as_ptr().cast());
    let target = PCSTR(c"cs_5_1".as_ptr().cast());
    let mut bytecode: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            PCSTR::null(),
            None,
            None::<&windows::Win32::Graphics::Direct3D::ID3DInclude>,
            entry,
            target,
            0,
            0,
            &mut bytecode,
            Some(&mut errors),
        )
    }
    .context("failed to compile BGRA to NCHW compute shader")?;
    bytecode.context("shader compiler returned no bytecode")
}

fn create_pipeline(device: &ID3D12Device) -> Result<(ID3D12RootSignature, ID3D12PipelineState)> {
    let root_signature = create_root_signature(device)?;
    let bytecode = compile_shader()?;
    let shader = D3D12_SHADER_BYTECODE {
        pShaderBytecode: unsafe { bytecode.GetBufferPointer() },
        BytecodeLength: unsafe { bytecode.GetBufferSize() },
    };
    let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
        pRootSignature: ManuallyDrop::new(Some(root_signature.clone())),
        CS: shader,
        NodeMask: 0,
        CachedPSO: D3D12_CACHED_PIPELINE_STATE::default(),
        Flags: D3D12_PIPELINE_STATE_FLAG_NONE,
    };
    let pipeline: ID3D12PipelineState = unsafe { device.CreateComputePipelineState(&desc) }
        .context("failed to create preprocess compute pipeline")?;
    Ok((root_signature, pipeline))
}

fn write_descriptors(
    device: &ID3D12Device,
    source: &ID3D12Resource,
    destination: &ID3D12Resource,
    format: DXGI_FORMAT,
    elements: usize,
    cpu_start: D3D12_CPU_DESCRIPTOR_HANDLE,
    descriptor_stride: u32,
) {
    let srv = D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D12_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
                PlaneSlice: 0,
                ResourceMinLODClamp: 0.0,
            },
        },
    };
    unsafe { device.CreateShaderResourceView(source, Some(&srv), cpu_start) };

    let uav = D3D12_UNORDERED_ACCESS_VIEW_DESC {
        Format: format,
        ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
        Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
            Buffer: D3D12_BUFFER_UAV {
                FirstElement: 0,
                NumElements: elements as u32,
                StructureByteStride: 0,
                CounterOffsetInBytes: 0,
                Flags: D3D12_BUFFER_UAV_FLAG_NONE,
            },
        },
    };
    unsafe {
        device.CreateUnorderedAccessView(
            destination,
            None::<&ID3D12Resource>,
            Some(&uav),
            offset_cpu(cpu_start, descriptor_stride),
        );
    }
}

fn offset_cpu(mut handle: D3D12_CPU_DESCRIPTOR_HANDLE, offset: u32) -> D3D12_CPU_DESCRIPTOR_HANDLE {
    handle.ptr += offset as usize;
    handle
}

fn offset_gpu(mut handle: D3D12_GPU_DESCRIPTOR_HANDLE, offset: u32) -> D3D12_GPU_DESCRIPTOR_HANDLE {
    handle.ptr += u64::from(offset);
    handle
}

#[cfg(test)]
pub(crate) fn preprocess_bgra_into(
    bgra: &[u8],
    width: usize,
    height: usize,
    output: &mut [f32],
) -> Result<()> {
    let pixels = width
        .checked_mul(height)
        .context("reference preprocess dimensions overflowed")?;
    if bgra.len() != pixels * 4 || output.len() != pixels * COLOR_CHANNELS {
        bail!("reference preprocess buffers do not match {width}x{height}");
    }
    for (index, pixel) in bgra.as_chunks::<4>().0.iter().enumerate() {
        output[index] = f32::from(pixel[2]) / 255.0;
        output[pixels + index] = f32::from(pixel[1]) / 255.0;
        output[pixels * 2 + index] = f32::from(pixel[0]) / 255.0;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{
        AcquiredDesktopFrame, CapturePoll, DxgiCapture, displays, gpu::create_shared_frame,
        init_thread,
    };
    use crate::sync::MutexExt;
    use windows::Win32::Graphics::Direct3D12::{
        D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
    };

    /// The GPU tests open the same desktop output, so they run one at a time.
    static GPU_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn create_tensor_buffer(
        gpu: &GpuDeviceContext,
        precision: Precision,
        elements: usize,
    ) -> Result<ID3D12Resource> {
        let properties = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: (precision.packed_words(elements).unwrap() * 4) as u64,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            ..Default::default()
        };
        let mut destination = None;
        unsafe {
            gpu.device().CreateCommittedResource(
                &properties,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                None,
                &mut destination,
            )
        }
        .context("failed to create test input buffer")?;
        destination.context("no test input buffer")
    }

    fn read_back(
        gpu: &mut GpuDeviceContext,
        buffer: &ID3D12Resource,
        byte_len: usize,
    ) -> Result<Vec<u8>> {
        let device = gpu.device().clone();
        let properties = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_READBACK,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: byte_len as u64,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let mut readback_resource = None;
        unsafe {
            device.CreateCommittedResource(
                &properties,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut readback_resource,
            )
        }
        .context("failed to create test readback buffer")?;
        let readback: ID3D12Resource = readback_resource.context("no test readback buffer")?;
        let allocator: ID3D12CommandAllocator =
            unsafe { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COMPUTE) }
                .context("failed to create test copy allocator")?;
        let list: ID3D12GraphicsCommandList = unsafe {
            device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_COMPUTE,
                &allocator,
                None::<&ID3D12PipelineState>,
            )
        }
        .context("failed to create test copy list")?;
        unsafe {
            list.ResourceBarrier(&[transition(
                buffer,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            list.CopyBufferRegion(&readback, 0, buffer, 0, byte_len as u64);
            list.Close().context("failed to close test copy list")?;
        }
        let list: ID3D12CommandList = list.cast()?;
        unsafe { gpu.queue().ExecuteCommandLists(&[Some(list)]) };
        gpu.wait_idle()?;
        let mut mapped: *mut std::ffi::c_void = std::ptr::null_mut();
        unsafe { readback.Map(0, None, Some(&mut mapped)) }
            .context("failed to map test readback")?;
        let bytes = unsafe { std::slice::from_raw_parts(mapped.cast::<u8>(), byte_len) }.to_vec();
        unsafe { readback.Unmap(0, None) };
        Ok(bytes)
    }

    /// The copy target must be bound first.
    fn acquire_fresh_image(capture: &mut DxgiCapture) -> Result<AcquiredDesktopFrame> {
        for _ in 0..50 {
            if let CapturePoll::Fresh(frame) = capture.acquire(20)? {
                return Ok(frame);
            }
        }
        bail!("no desktop image acquired within 50 polls")
    }

    #[test]
    #[ignore = "needs an interactive desktop session and a GPU"]
    fn single_frame_gpu_transport_can_be_reused() -> Result<()> {
        let _serial = GPU_TESTS.lock_recover();
        init_thread()?;
        let display = displays()?
            .into_iter()
            .next()
            .context("no desktop display available")?;
        let mut capture = DxgiCapture::new(display.adapter_index, display.output_index)?;
        let mut gpu = GpuDeviceContext::new(display.adapter_luid)?;
        let size = (32, 32);

        for precision in [Precision::Fp32, Precision::Fp16] {
            let (capture_frame, inference_frame) =
                create_shared_frame(&gpu, &capture.transport_device(), size.0, size.1)?;
            capture.bind_copy_target(&capture_frame, size)?;
            let image = acquire_fresh_image(&mut capture)?;
            let destination =
                create_tensor_buffer(&gpu, precision, size.0 * size.1 * COLOR_CHANNELS)?;
            let mut preprocess = GpuPreprocessor::new(
                &gpu,
                &inference_frame.resource,
                &destination,
                size.0,
                size.1,
                precision,
            )?;
            // Wait before resources are released even when a submission/assertion fails.
            let result = (|| -> Result<()> {
                for seq in 1..=32 {
                    let completed = inference_frame.reuse_completed();
                    anyhow::ensure!(
                        completed != u64::MAX && completed >= seq - 1,
                        "shared texture reused before preprocess completion"
                    );
                    let measure = seq % 2 == 0;
                    capture.copy_to_target(&image, seq, measure)?;
                    let ticket = preprocess.submit(
                        seq,
                        &inference_frame.capture_ready_fence,
                        &inference_frame.reuse_fence,
                        completed,
                        measure,
                    )?;
                    gpu.wait_idle()?;
                    let timing = preprocess.collect(ticket, &inference_frame.reuse_fence)?;
                    if ticket.timed {
                        anyhow::ensure!(
                            timing.is_some_and(|ms| ms.is_finite() && ms >= 0.0),
                            "invalid GPU timing"
                        );
                    } else {
                        anyhow::ensure!(timing.is_none(), "untimed frame produced a timing");
                    }
                }
                Ok(())
            })();
            let drained = gpu.wait_idle();
            result?;
            drained?;
        }
        Ok(())
    }

    #[test]
    #[ignore = "needs an interactive desktop session and a GPU"]
    fn gpu_preprocess_matches_the_reference_for_fp32_and_fp16_with_an_odd_width() -> Result<()> {
        let _serial = GPU_TESTS.lock_recover();
        init_thread()?;
        let display = displays()?
            .into_iter()
            .next()
            .context("no desktop display available")?;
        let capture = DxgiCapture::new(display.adapter_index, display.output_index)?;
        let mut gpu = GpuDeviceContext::new(display.adapter_luid)?;
        let (width, height) = (33, 9);
        let pixels = width * height;
        let elements = pixels * COLOR_CHANNELS;
        let bgra: Vec<u8> = (0..pixels)
            .flat_map(|index| {
                [
                    (index * 7 % 256) as u8,
                    (index * 13 % 256) as u8,
                    (index * 31 % 256) as u8,
                    255,
                ]
            })
            .collect();
        let mut expected = vec![0.0; elements];
        preprocess_bgra_into(&bgra, width, height, &mut expected)?;

        for precision in [Precision::Fp32, Precision::Fp16] {
            let (capture_frame, inference_frame) =
                create_shared_frame(&gpu, &capture.transport_device(), width, height)?;
            capture.write_test_image(&capture_frame, &bgra, (width, height), 1)?;
            let destination = create_tensor_buffer(&gpu, precision, elements)?;
            let mut preprocess = GpuPreprocessor::new(
                &gpu,
                &inference_frame.resource,
                &destination,
                width,
                height,
                precision,
            )?;
            let result = (|| -> Result<Vec<f32>> {
                let ticket = preprocess.submit(
                    1,
                    &inference_frame.capture_ready_fence,
                    &inference_frame.reuse_fence,
                    inference_frame.reuse_completed(),
                    false,
                )?;
                gpu.wait_idle()?;
                preprocess.collect(ticket, &inference_frame.reuse_fence)?;
                let bytes =
                    read_back(&mut gpu, &destination, elements * precision.element_bytes())?;
                Ok(match precision {
                    Precision::Fp32 => bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|chunk| f32::from_le_bytes(*chunk))
                        .collect(),
                    Precision::Fp16 => bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|chunk| half::f16::from_le_bytes(*chunk).to_f32())
                        .collect(),
                })
            })();
            let drained = gpu.wait_idle();
            let actual = result?;
            drained?;
            let tolerance = match precision {
                Precision::Fp32 => 1e-6,
                Precision::Fp16 => 1e-3,
            };
            for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
                anyhow::ensure!(
                    (actual - expected).abs() <= tolerance,
                    "{} tensor element {index}: got {actual}, expected {expected}",
                    precision.label()
                );
            }
        }
        Ok(())
    }
}
