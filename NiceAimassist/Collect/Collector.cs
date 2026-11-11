using System.Buffers;
using NiceAimassist.Capture;
using NiceAimassist.Detect;
using NiceAimassist.Shared.Gpu;
using Silk.NET.Direct3D11;

namespace NiceAimassist.Collect;

/// <summary>
/// Copies the desktop into staging textures, maps them, and hands them to the writer thread.
/// Every method except the writer callbacks runs on the loop thread. The loop thread never waits on the GPU.
/// </summary>
public sealed unsafe class Collector : ICollectControl, IFrameCollector, IDisposable
{
    private readonly CollectState _state;
    private readonly StagingCopy _copy;
    private readonly SampleQueue _queue;
    private readonly DatasetWriter _dataset;
    private int _size = CollectSettings.DefaultSize;
    private string _pendingFileName = string.Empty;
    private bool _disposed;

    public Collector(GpuDevices gpu, string datasetRoot, Func<long> now, long ticksPerSecond)
    {
        _dataset = new DatasetWriter(datasetRoot);
        _state = new CollectState(now, ticksPerSecond);
        _copy = new StagingCopy(gpu);
        _queue = new SampleQueue(Write, error => _state.ReportWriteError(error.Message));
    }

    // While the staging textures are mapped, a new GPU copy would write memory that the writer still reads.
    public bool IsDue => _state.IsDue && _copy.CanCopy;

    public CollectStatus Update(CollectSettings settings, bool held)
    {
        _state.SetEnabled(held && settings.Enabled);
        _size = settings.Size;
        _state.SetRate(settings.Rate);
        _copy.ReleaseIfCopied();
        SubmitReadySample();
        return new CollectStatus(_state.Active, _queue.Saved, _state.LastError);
    }

    public void Collect(ID3D11Texture2D* desktop)
    {
        try
        {
            _copy.Copy(desktop, _size);
            _pendingFileName = DatasetWriter.FileNameFor(DateTime.Now);
            _state.MarkPending();
        }
        catch (Exception error)
        {
            _state.Fail(error.Message);
        }
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        _state.SetEnabled(false);
        _queue.Dispose();
        _copy.Dispose();
    }

    private static void Release(BgraImage image) => ArrayPool<byte>.Shared.Return(image.Pixels);

    private void SubmitReadySample()
    {
        if (!_state.Pending)
        {
            return;
        }

        try
        {
            if (!_copy.TryMap(out MappedPixels full, out MappedPixels crop))
            {
                return;
            }

            _state.ClearPending();
            bool accepted = false;
            try
            {
                accepted = _queue.TrySubmit(new Sample(_pendingFileName, crop, full));
            }
            finally
            {
                if (!accepted)
                {
                    _copy.CancelMap();
                }
            }
        }
        catch (Exception error)
        {
            _state.Fail(error.Message);
        }
    }

    private void Write(Sample sample)
    {
        BgraImage crop = default;
        BgraImage original = default;
        try
        {
            try
            {
                crop = sample.Crop.CopyToPooled();
                original = sample.Original.CopyToPooled();
            }
            finally
            {
                _copy.MarkCopied();
            }

            _dataset.Write(sample.FileNameWithoutExtension, crop, original);
        }
        finally
        {
            if (crop.Pixels is not null)
            {
                Release(crop);
            }

            if (original.Pixels is not null)
            {
                Release(original);
            }
        }
    }
}
