using System.Diagnostics;
using Pstm.Shared.Gpu;
using Silk.NET.Core.Native;
using Silk.NET.Direct3D12;
using Silk.NET.DXGI;

namespace Pstm.Detect.Runtime;

// The timestamps are valid after the fence of the list that calls RecordEnd completes.
// The command lists of the Preprocessor and the OutputReader write into this heap. Release them before this object.
internal sealed unsafe class GpuTimer : IDisposable
{
    private const uint StartIndex = 0;
    private const uint EndIndex = 1;
    private const uint QueryCount = 2;
    private const ulong ReadbackBytes = QueryCount * sizeof(ulong);

    private readonly ulong _gpuFrequency;
    private ID3D12QueryHeap* _heap;
    private ID3D12Resource* _readback;
    private ulong* _values;
    private bool _disposed;

    public GpuTimer(GpuDevices gpu)
    {
        ArgumentNullException.ThrowIfNull(gpu);
        ID3D12Device* device = gpu.Device;
        try
        {
            ulong frequency;
            ComInterop.Check(gpu.ComputeQueue->GetTimestampFrequency(&frequency), "GetTimestampFrequency (compute queue)");
            if (frequency == 0)
            {
                throw new InvalidOperationException("GetTimestampFrequency returned 0 for the compute queue.");
            }

            _gpuFrequency = frequency;

            QueryHeapDesc heapDesc = new() { Type = QueryHeapType.Timestamp, Count = QueryCount, NodeMask = 0 };
            ID3D12QueryHeap* heap;
            ComInterop.Check(
                device->CreateQueryHeap(&heapDesc, ComInterop.IidOf<ID3D12QueryHeap>(), (void**)&heap),
                "CreateQueryHeap (timestamp)");
            _heap = heap;

            HeapProperties properties = new() { Type = HeapType.Readback, CreationNodeMask = 1, VisibleNodeMask = 1 };
            ResourceDesc description = new()
            {
                Dimension = ResourceDimension.Buffer,
                Width = ReadbackBytes,
                Height = 1,
                DepthOrArraySize = 1,
                MipLevels = 1,
                Format = Format.FormatUnknown,
                SampleDesc = new SampleDesc(1, 0),
                Layout = TextureLayout.LayoutRowMajor,
                Flags = ResourceFlags.None,
            };
            ID3D12Resource* readback;
            ComInterop.Check(
                device->CreateCommittedResource(
                    &properties, HeapFlags.None, &description, ResourceStates.CopyDest, null, ComInterop.IidOf<ID3D12Resource>(), (void**)&readback),
                "CreateCommittedResource (timestamp readback)");
            _readback = readback;

            void* mapped;
            ComInterop.Check(_readback->Map(0, null, &mapped), "Map the timestamp readback buffer");
            _values = (ulong*)mapped;
        }
        catch
        {
            ReleaseAll();
            throw;
        }
    }

    internal void RecordStart(ID3D12GraphicsCommandList* list)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        list->EndQuery(_heap, QueryType.Timestamp, StartIndex);
    }

    internal void RecordEnd(ID3D12GraphicsCommandList* list)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        list->EndQuery(_heap, QueryType.Timestamp, EndIndex);
        list->ResolveQueryData(_heap, QueryType.Timestamp, StartIndex, QueryCount, _readback, 0);
    }

    /// <summary>The GPU time between the two timestamps, in Stopwatch ticks. Call it after the fence of the RecordEnd list.</summary>
    internal long ReadElapsedStopwatchTicks()
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        return ToStopwatchTicks(_values[StartIndex], _values[EndIndex], _gpuFrequency, Stopwatch.Frequency);
    }

    internal static long ToStopwatchTicks(ulong start, ulong end, ulong gpuFrequency, long stopwatchFrequency)
    {
        if (end < start)
        {
            return 0;
        }

        // The product can exceed 64 bits. UInt128 keeps the division exact.
        return (long)((UInt128)(end - start) * (ulong)stopwatchFrequency / gpuFrequency);
    }

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
        if (_readback != null && _values != null)
        {
            _readback->Unmap(0, null);
            _values = null;
        }

        ComInterop.Release(ref _readback);
        ComInterop.Release(ref _heap);
    }
}
