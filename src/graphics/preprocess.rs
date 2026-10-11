use std::mem::ManuallyDrop;

use super::gpu_diagnostics::{GpuFaultStage, OWN_OBJECT_PREFIX, name_d3d12};
use super::profile_sampler::ProfileSampler;
use super::{device::GpuDeviceContext, frame::GpuFrameMetadata};
use crate::inference::TensorPrecision;
use anyhow::{Context, Result, bail};
use windows::{
    Win32::Graphics::{
        Direct3D::Fxc::D3DCompile,
        Direct3D::{D3D_SHADER_MACRO, ID3DBlob},
        Direct3D12::*,
        Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R32_TYPELESS},
    },
    core::{Interface, PCSTR},
};

/// BGRA texture -> planar RGB (NCHW) buffer. Each thread writes one 32-bit
/// word: one FP32 element, or two packed FP16 elements (`FP16` defined).
/// Working on the flat element index keeps odd-sized planes correct.
const SHADER: &str = r#"
Texture2D<float4> source_image : register(t0);
RWByteAddressBuffer destination : register(u0);
cbuffer Params : register(b0) { uint width; uint pixels; uint elements; };
float element(uint index) {
    float value = 0.0;
    if (index < elements) {
        uint channel = index / pixels;
        uint pixel = index - channel * pixels;
        float4 color = source_image.Load(int3(pixel % width, pixel / width, 0));
        value = channel == 0 ? color.r : (channel == 1 ? color.g : color.b);
    }
    return value;
}
[numthreads(256, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
#ifdef FP16
    uint first = id.x * 2;
    if (first >= elements) return;
    destination.Store(id.x * 4, f32tof16(element(first)) | (f32tof16(element(first + 1)) << 16));
#else
    if (id.x >= elements) return;
    destination.Store(id.x * 4, asuint(element(id.x)));
#endif
}
"#;
const COLOR_CHANNELS: usize = 3;
const PREPROCESS_PARAMETER_COUNT: u32 = 3;
const THREADS_PER_GROUP: usize = 256;
const MAX_DISPATCH_GROUPS: usize = 65_535;

/// Field order is drop order: the recorded lists go before everything they refer to.
pub struct GpuPreprocessor {
    /// Recorded once: shapes and descriptors are static,
    /// so a frame only needs to be submitted.
    command_list: ID3D12GraphicsCommandList,
    /// Same dispatch without timestamp queries, for frames that are not measured.
    untimed_list: ID3D12GraphicsCommandList,
    // Objects the recorded lists refer to.
    _allocator: ID3D12CommandAllocator,
    _descriptor_heap: ID3D12DescriptorHeap,
    _pipeline: ID3D12PipelineState,
    _root_signature: ID3D12RootSignature,
    _destination: ID3D12Resource,
    _source: ID3D12Resource,
    queue: ID3D12CommandQueue,
    /// Completion value after which the lists may be executed again.
    list_in_flight: Option<u64>,
    timestamps: PreprocessTimestamps,
    sampler: ProfileSampler,
    fault_stage: GpuFaultStage,
}

#[derive(Debug, Clone, Copy)]
pub struct PreprocessTicket {
    pub completion_value: u64,
    pub frame_seq: u64,
    /// Whether this submission recorded timestamps.
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
        precision: TensorPrecision,
    ) -> Result<Self> {
        let device = gpu.device();
        if width == 0 || height == 0 || width > u32::MAX as usize || height > u32::MAX as usize {
            bail!("invalid GPU preprocess dimensions {width}x{height}");
        }
        let pixels = width.checked_mul(height).context("input size overflowed")?;
        let elements = pixels
            .checked_mul(COLOR_CHANNELS)
            .context("input tensor size overflowed")?;
        let words = precision
            .packed_words(elements)
            .context("input tensor size overflowed")?;
        let dispatch_groups = words.div_ceil(THREADS_PER_GROUP);
        if words > u32::MAX as usize || dispatch_groups > MAX_DISPATCH_GROUPS {
            bail!("input {width}x{height} is too large for the GPU preprocess dispatch");
        }

        let (root_signature, pipeline) = create_pipeline(device, precision)?;
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
        let timestamps = PreprocessTimestamps::new(device)?;
        name_d3d12(&pipeline, &format!("{OWN_OBJECT_PREFIX}preprocess-pso"));
        name_d3d12(
            &root_signature,
            &format!("{OWN_OBJECT_PREFIX}preprocess-root"),
        );
        name_d3d12(
            &descriptor_heap,
            &format!("{OWN_OBJECT_PREFIX}preprocess-heap"),
        );
        name_d3d12(
            &allocator,
            &format!("{OWN_OBJECT_PREFIX}preprocess-allocator"),
        );

        let cpu_base = unsafe { descriptor_heap.GetCPUDescriptorHandleForHeapStart() };
        {
            let cpu = cpu_base;
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
            unsafe { device.CreateShaderResourceView(source, Some(&srv), cpu) };
        }
        let output_cpu = offset_cpu(cpu_base, descriptor_stride);
        let uav = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R32_TYPELESS,
            ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
            Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                Buffer: D3D12_BUFFER_UAV {
                    FirstElement: 0,
                    NumElements: words as u32,
                    StructureByteStride: 0,
                    CounterOffsetInBytes: 0,
                    Flags: D3D12_BUFFER_UAV_FLAG_RAW,
                },
            },
        };
        unsafe {
            device.CreateUnorderedAccessView(
                destination,
                None::<&ID3D12Resource>,
                Some(&uav),
                output_cpu,
            )
        };

        let gpu_base = unsafe { descriptor_heap.GetGPUDescriptorHandleForHeapStart() };
        let output_gpu = offset_gpu(gpu_base, descriptor_stride);
        let constants = [width as u32, pixels as u32, elements as u32];
        let record = |timed: bool| -> Result<ID3D12GraphicsCommandList> {
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
                dispatch_groups: dispatch_groups as u32,
                timed,
            }
            .record()?;
            let kind = if timed { "timed" } else { "untimed" };
            name_d3d12(&list, &format!("{OWN_OBJECT_PREFIX}preprocess-list-{kind}"));
            Ok(list)
        };
        let command_list = record(true)?;
        let untimed_list = record(false)?;

        Ok(Self {
            command_list,
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
            sampler: ProfileSampler::default(),
            fault_stage: GpuFaultStage::PreprocessSubmit,
        })
    }

    pub fn submit(
        &mut self,
        metadata: GpuFrameMetadata,
        ready_fence: &ID3D12Fence,
        reuse_fence: &ID3D12Fence,
    ) -> Result<PreprocessTicket> {
        self.fault_stage = GpuFaultStage::PreprocessSubmit;
        let reuse_value = metadata.capture_ready_value;
        if self.timestamps.frequency == 0 {
            self.timestamps.frequency = unsafe { self.queue.GetTimestampFrequency() }
                .context("failed to query compute timestamp frequency")?;
        }
        let completed = unsafe { reuse_fence.GetCompletedValue() };
        if completed == u64::MAX {
            self.fault_stage = GpuFaultStage::DeviceRemoved;
            bail!("D3D12 reuse fence reports device removed");
        }
        if self.list_in_flight.is_some_and(|value| completed < value) {
            self.fault_stage = GpuFaultStage::PreprocessSubmit;
            bail!("preprocess command list reused before its reuse fence completed");
        }
        self.fault_stage = GpuFaultStage::D3D12ReadyWait;
        if let Err(error) = unsafe { self.queue.Wait(ready_fence, metadata.capture_ready_value) } {
            return Err(error).context("D3D12 queue wait on capture fence failed");
        }
        self.fault_stage = GpuFaultStage::PreprocessSubmit;
        let timed = self.sampler.should_measure();
        if timed {
            self.timestamps.pending = Some(reuse_value);
        }
        let list = if timed {
            &self.command_list
        } else {
            &self.untimed_list
        };
        let command_list = match list.cast() {
            Ok(command_list) => command_list,
            Err(error) => {
                return Err(error).context("failed to cast compute command list");
            }
        };
        unsafe { self.queue.ExecuteCommandLists(&[Some(command_list)]) };
        self.fault_stage = GpuFaultStage::ReuseSignal;
        if let Err(error) = unsafe { self.queue.Signal(reuse_fence, reuse_value) } {
            return Err(error).context("D3D12 queue signal on reuse fence failed");
        }
        self.list_in_flight = Some(reuse_value);
        Ok(PreprocessTicket {
            completion_value: reuse_value,
            frame_seq: metadata.frame_seq,
            timed,
        })
    }

    /// Returns the GPU time of a measured frame, `None` for an unmeasured one.
    pub fn collect(
        &mut self,
        ticket: PreprocessTicket,
        reuse_fence: &ID3D12Fence,
    ) -> Result<Option<f64>> {
        let completed = unsafe { reuse_fence.GetCompletedValue() };
        if completed == u64::MAX {
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
        if !ticket.timed {
            return Ok(None);
        }
        self.timestamps.collect(ticket.completion_value).map(Some)
    }

    pub fn fault_stage(&self) -> GpuFaultStage {
        self.fault_stage
    }
}

/// Everything needed to record the preprocess dispatch.
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
    dispatch_groups: u32,
    /// Bracket the dispatch with timestamp queries and resolve them.
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
            list.Dispatch(self.dispatch_groups, 1, 1);
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
    fn new(device: &ID3D12Device) -> Result<Self> {
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
            frequency: 0,
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
        Ok((values[1] - values[0]) as f64 * 1000.0 / self.frequency as f64)
    }
}

impl Drop for PreprocessTimestamps {
    fn drop(&mut self) {
        let empty = D3D12_RANGE { Begin: 0, End: 0 };
        unsafe { self.readback.Unmap(0, Some(&empty)) };
    }
}

fn create_pipeline(
    device: &ID3D12Device,
    precision: TensorPrecision,
) -> Result<(ID3D12RootSignature, ID3D12PipelineState)> {
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
    let root_signature: ID3D12RootSignature =
        unsafe { device.CreateRootSignature(0, signature_bytes) }
            .context("failed to create preprocess root signature")?;

    let source = SHADER.as_bytes();
    let entry = PCSTR(c"main".as_ptr().cast());
    let target = PCSTR(c"cs_5_1".as_ptr().cast());
    let mut bytecode: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let fp16_define = [
        D3D_SHADER_MACRO {
            Name: PCSTR(c"FP16".as_ptr().cast()),
            Definition: PCSTR(c"1".as_ptr().cast()),
        },
        D3D_SHADER_MACRO::default(),
    ];
    let defines = (precision == TensorPrecision::Fp16).then_some(fp16_define.as_ptr());
    unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            PCSTR::null(),
            defines,
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
    let bytecode = bytecode.context("shader compiler returned no bytecode")?;
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

fn transition(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
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
        output[index] = pixel[2] as f32 / 255.0;
        output[pixels + index] = pixel[1] as f32 / 255.0;
        output[pixels * 2 + index] = pixel[0] as f32 / 255.0;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs an interactive desktop session and a GPU"]
    fn single_frame_gpu_transport_can_be_reused() -> Result<()> {
        use crate::graphics::{CapturePoll, GraphicsThread, shared_frame::create_shared_frame};

        let graphics = GraphicsThread::initialize()?;
        let display = graphics
            .enumerate_displays()?
            .into_iter()
            .next()
            .context("no desktop display available")?;
        let mut capture = graphics.create_capture(display.adapter_index, display.output_index)?;
        let mut gpu = GpuDeviceContext::new(display.adapter_luid)?;
        let size = (32, 32);
        let mut image = None;
        for _ in 0..50 {
            if let CapturePoll::Fresh(frame) = capture.acquire(20)? {
                image = Some(capture.cache_frame(&frame, size)?);
                break;
            }
        }
        let image = image.context("no desktop image acquired within 50 polls")?;

        for precision in [TensorPrecision::Fp32, TensorPrecision::Fp16] {
            let (capture_frame, inference_frame) =
                create_shared_frame(&gpu, &capture.transport_device(), size.0, size.1)?;
            let properties = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                CreationNodeMask: 1,
                VisibleNodeMask: 1,
                ..Default::default()
            };
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: (precision
                    .packed_words(size.0 * size.1 * COLOR_CHANNELS)
                    .unwrap()
                    * 4) as u64,
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
            let destination: ID3D12Resource = destination.context("no test input buffer")?;
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
                    let completed = unsafe { inference_frame.reuse_fence.GetCompletedValue() };
                    anyhow::ensure!(
                        completed != u64::MAX && completed >= seq - 1,
                        "shared texture reused before preprocess completion"
                    );
                    let submit = capture.copy_center_to(
                        &image,
                        &capture_frame.texture,
                        &capture_frame.capture_ready_fence,
                        seq,
                        size,
                    )?;
                    let metadata = GpuFrameMetadata {
                        pipeline_id: 1,
                        generation: 1,
                        frame_seq: seq,
                        source_seq: 1,
                        reused: seq > 1,
                        present_qpc: submit.present_qpc,
                        ready_qpc: submit.present_qpc,
                        capture_ready_value: seq,
                        accumulated_frames: submit.accumulated_frames,
                        desktop_size: submit.desktop_size,
                        acquire_wait_ms: submit.acquire_wait_ms,
                        capture_ms: submit.capture_ms,
                        copy_submit_ms: submit.copy_submit_ms,
                    };
                    let ticket = preprocess.submit(
                        metadata,
                        &inference_frame.capture_ready_fence,
                        &inference_frame.reuse_fence,
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
    fn reference_matches_bgra_texture_channel_order_and_nchw_layout() {
        let bgra = [0, 64, 255, 255, 128, 32, 16, 255];
        let mut output = [0.0; 2 * COLOR_CHANNELS];
        preprocess_bgra_into(&bgra, 2, 1, &mut output).unwrap();
        let expected = [
            1.0,
            16.0 / 255.0,
            64.0 / 255.0,
            32.0 / 255.0,
            0.0,
            128.0 / 255.0,
        ];
        for (actual, expected) in output.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-6);
        }
    }
}
