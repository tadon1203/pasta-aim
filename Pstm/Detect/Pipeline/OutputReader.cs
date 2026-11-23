using Pstm.Detect.Runtime;
using Pstm.Shared.Gpu;
using Silk.NET.Direct3D12;
using Silk.NET.DXGI;

namespace Pstm.Detect.Pipeline;

// The copy is recorded on the compute queue. It runs after any model run submitted to that queue.
internal sealed unsafe class OutputReader : IDisposable
{
    private readonly ID3D12CommandQueue* _queue;
    private readonly byte* _mapped;
    private readonly int _length;
    private ID3D12CommandAllocator* _allocator;
    private ID3D12GraphicsCommandList* _list;
    private ID3D12Resource* _buffer;
    private ID3D12Fence* _fence;
    private ulong _fenceValue;
    private bool _disposed;

    // This object does not own the source. The source must stay alive, in the unordered access state, for the life of this object.
    public OutputReader(GpuDevices gpu, ID3D12Resource* source, int length, GpuTimer timer)
    {
        ID3D12Device* device = gpu.Device;
        if (length <= 0 || source->GetDesc().Width < (ulong)length)
        {
            throw new ArgumentException(
                $"The output copy of {length} bytes does not fit its source of {source->GetDesc().Width} bytes.",
                nameof(length));
        }

        _queue = gpu.ComputeQueue;
        _length = length;

        try
        {
            HeapProperties heap = new() { Type = HeapType.Readback, CreationNodeMask = 1, VisibleNodeMask = 1 };
            ResourceDesc description = new()
            {
                Dimension = ResourceDimension.Buffer,
                Width = (ulong)length,
                Height = 1,
                DepthOrArraySize = 1,
                MipLevels = 1,
                Format = Format.FormatUnknown,
                SampleDesc = new SampleDesc(1, 0),
                Layout = TextureLayout.LayoutRowMajor,
                Flags = ResourceFlags.None,
            };
            ID3D12Resource* buffer;
            ComInterop.Check(
                device->CreateCommittedResource(
                    &heap, HeapFlags.None, &description, ResourceStates.CopyDest, null, ComInterop.IidOf<ID3D12Resource>(), (void**)&buffer),
                "CreateCommittedResource (output copy)");
            _buffer = buffer;

            void* mapped;
            ComInterop.Check(_buffer->Map(0, null, &mapped), "Map the output copy buffer");
            _mapped = (byte*)mapped;

            ID3D12CommandAllocator* allocator;
            ComInterop.Check(
                device->CreateCommandAllocator(CommandListType.Compute, ComInterop.IidOf<ID3D12CommandAllocator>(), (void**)&allocator),
                "CreateCommandAllocator (output copy)");
            _allocator = allocator;

            ID3D12GraphicsCommandList* list;
            ComInterop.Check(
                device->CreateCommandList(
                    0, CommandListType.Compute, _allocator, null, ComInterop.IidOf<ID3D12GraphicsCommandList>(), (void**)&list),
                "CreateCommandList (output copy)");
            _list = list;

            // This list runs after the model work on the queue. The end query must come first in the list.
            timer.RecordEnd(_list);
            ResourceBarrier toCopy = D3D12Barrier.Transition(source, ResourceStates.UnorderedAccess, ResourceStates.CopySource);
            _list->ResourceBarrier(1, &toCopy);
            _list->CopyBufferRegion(_buffer, 0, source, 0, (ulong)length);
            ResourceBarrier toUnordered = D3D12Barrier.Transition(source, ResourceStates.CopySource, ResourceStates.UnorderedAccess);
            _list->ResourceBarrier(1, &toUnordered);
            ComInterop.Check(_list->Close(), "Close the output copy command list");

            ID3D12Fence* fence;
            ComInterop.Check(device->CreateFence(0, FenceFlags.None, ComInterop.IidOf<ID3D12Fence>(), (void**)&fence), "CreateFence (output copy)");
            _fence = fence;
        }
        catch
        {
            ReleaseAll();
            throw;
        }
    }

    // The span is valid until the next Read.
    public ReadOnlySpan<byte> Read()
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        ID3D12CommandList* list = (ID3D12CommandList*)_list;
        _queue->ExecuteCommandLists(1, &list);
        ulong value = ++_fenceValue;
        ComInterop.Check(_queue->Signal(_fence, value), "Signal the output copy fence");
        FenceSpin.Wait(_fence, value);
        return new ReadOnlySpan<byte>(_mapped, _length);
    }

    // The queue must be idle before this runs. The pipeline waits for it first.
    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        ReleaseAll();
    }

    private void ReleaseAll()
    {
        if (_buffer != null && _mapped != null)
        {
            _buffer->Unmap(0, null);
        }

        ComInterop.Release(ref _fence);
        ComInterop.Release(ref _list);
        ComInterop.Release(ref _allocator);
        ComInterop.Release(ref _buffer);
    }
}
