//! Reads a small GPU buffer back to the CPU on the compute queue. The model output goes through it.
//!
//! The output of the model stays on the GPU, so that ONNX Runtime can replay a captured graph with
//! the same buffers on each run. The copy to the CPU is ours: one recorded command list, and a fence
//! that the CPU waits for.

use anyhow::{Context, Result, bail};
use windows::Win32::Graphics::{
    Direct3D12::{
        D3D12_COMMAND_LIST_TYPE_COMPUTE, D3D12_CPU_PAGE_PROPERTY_UNKNOWN, D3D12_HEAP_FLAG_NONE,
        D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_READBACK, D3D12_MEMORY_POOL_UNKNOWN, D3D12_RANGE,
        D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_BUFFER, D3D12_RESOURCE_FLAG_NONE,
        D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE,
        D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        ID3D12CommandAllocator, ID3D12CommandList, ID3D12CommandQueue, ID3D12GraphicsCommandList,
        ID3D12PipelineState, ID3D12Resource,
    },
    Dxgi::Common::{DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC},
};
use windows::core::Interface;

use super::gpu::{GpuDeviceContext, QueueFence, transition};

pub(super) struct Readback {
    /// Recorded once: the barriers and the copy. It runs again after each wait.
    list: ID3D12CommandList,
    _allocator: ID3D12CommandAllocator,
    /// Kept so that the recorded list never outlives the buffer that it reads.
    _source: ID3D12Resource,
    buffer: ID3D12Resource,
    /// The whole readback buffer, mapped for as long as `Readback` lives.
    mapped: *const u8,
    len: usize,
    queue: ID3D12CommandQueue,
    fence: QueueFence,
    /// Wait in a busy loop and do not sleep.
    spin: bool,
}

impl Readback {
    /// Prepares the copy of the first `len` bytes of `source`. The buffer must be in the unordered
    /// access state, as the buffers of `DirectML` are, and it is back in that state after each copy.
    pub(super) fn new(
        gpu: &GpuDeviceContext,
        source: &ID3D12Resource,
        len: usize,
        spin: bool,
    ) -> Result<Self> {
        let device = gpu.device();
        let source_size = unsafe { source.GetDesc() }.Width;
        if len == 0 || source_size < len as u64 {
            bail!("the readback of {len} bytes does not fit its source of {source_size} bytes");
        }
        let properties = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_READBACK,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0,
            Width: len as u64,
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
        let mut buffer = None;
        // SAFETY: the descriptors are valid, and a readback buffer starts in the copy destination state.
        unsafe {
            device.CreateCommittedResource(
                &properties,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut buffer,
            )
        }
        .context("failed to create the readback buffer")?;
        let buffer: ID3D12Resource = buffer.context("the readback buffer was not created")?;
        let range = D3D12_RANGE { Begin: 0, End: len };
        let mut mapped = std::ptr::null_mut();
        unsafe { buffer.Map(0, Some(&range), Some(&mut mapped)) }
            .context("failed to map the readback buffer")?;

        let allocator: ID3D12CommandAllocator =
            unsafe { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COMPUTE) }
                .context("failed to create the readback command allocator")?;
        let list: ID3D12GraphicsCommandList = unsafe {
            device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_COMPUTE,
                &allocator,
                None::<&ID3D12PipelineState>,
            )
        }
        .context("failed to create the readback command list")?;
        // SAFETY: the buffers are live, and `len` bytes fit both of them.
        unsafe {
            list.ResourceBarrier(&[transition(
                source,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            )]);
            list.CopyBufferRegion(&buffer, 0, source, 0, len as u64);
            list.ResourceBarrier(&[transition(
                source,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            )]);
            list.Close()
        }
        .context("failed to close the readback command list")?;

        Ok(Self {
            list: list
                .cast()
                .context("failed to cast the readback command list")?,
            _allocator: allocator,
            _source: source.clone(),
            buffer,
            mapped: mapped.cast::<u8>().cast_const(),
            len,
            queue: gpu.queue().clone(),
            fence: QueueFence::new(device, "readback")?,
            spin,
        })
    }

    /// Waits until all the work that is already on the queue is done.
    pub(super) fn drain(&mut self) -> Result<()> {
        self.fence.signal_and_wait(&self.queue, false)
    }

    /// Copies the source after all the work that is already on the queue, and waits for the copy. The
    /// bytes are valid until the next call.
    pub(super) fn read(&mut self) -> Result<&[u8]> {
        // SAFETY: the list is closed, and the wait of the last read ended its previous run.
        unsafe { self.queue.ExecuteCommandLists(&[Some(self.list.clone())]) };
        self.fence
            .signal_and_wait(&self.queue, self.spin)
            .context("failed to read the model output back")?;
        // SAFETY: `mapped` points to `len` bytes of the readback buffer, and the copy filled them
        // before the fence passed.
        Ok(unsafe { std::slice::from_raw_parts(self.mapped, self.len) })
    }
}

impl Drop for Readback {
    fn drop(&mut self) {
        let empty = D3D12_RANGE { Begin: 0, End: 0 };
        // SAFETY: the buffer was mapped in `new`, and the queue is idle before the pipeline drops it.
        unsafe { self.buffer.Unmap(0, Some(&empty)) };
    }
}
