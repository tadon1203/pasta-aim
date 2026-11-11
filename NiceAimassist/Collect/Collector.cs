using System.Buffers;
using NiceAimassist.Capture;
using NiceAimassist.Display;
using Silk.NET.Direct3D11;

namespace NiceAimassist.Collect;

/// <summary>
/// Copies the desktop into staging textures. Reads them back later. The loop thread never waits on the GPU.
/// Every method except the writer callbacks runs on the loop thread.
/// </summary>
public sealed unsafe class Collector : ICollectControl, IFrameObserver, IDisposable
{
    private readonly CollectState _state;
    private readonly CollectCopy _copy;
    private readonly SampleQueue _queue;
    private readonly DatasetWriter _dataset;
    private int _size = CollectSettings.DefaultSize;
    private string _pendingFileName = string.Empty;
    private bool _disposed;

    public Collector(DisplayDevices gpu, string datasetRoot, Func<long> now, long ticksPerSecond)
    {
        _dataset = new DatasetWriter(datasetRoot);
        _state = new CollectState(new CollectSchedule(now, ticksPerSecond));
        _copy = new CollectCopy(gpu);
        _queue = new SampleQueue(Write, error => _state.ReportWriteError(error.Message));
    }

    public bool IsDue => _state.IsDue;

    public CollectStatus Update(CollectSettings settings, bool held)
    {
        _state.SetEnabled(held && settings.Enabled);
        _size = settings.Size;
        _state.SetRate(settings.Rate);
        SubmitReadySample();
        return new CollectStatus(_state.Active, _queue.Saved, _state.LastError);
    }

    public void Observe(ID3D11Texture2D* desktop)
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
            if (!_copy.TryReadFull(out BgraImage original))
            {
                return;
            }

            _state.ClearPending();
            if (!_copy.TryReadCrop(out BgraImage crop))
            {
                Release(original);
                _state.Fail("the crop staging texture was not readable");
                return;
            }

            if (!_queue.TrySubmit(new Sample(_pendingFileName, crop, original)))
            {
                Release(crop);
                Release(original);
            }
        }
        catch (Exception error)
        {
            _state.Fail(error.Message);
        }
    }

    private void Write(Sample sample)
    {
        try
        {
            _dataset.Write(sample.FileNameWithoutExtension, sample.Crop, sample.Original);
        }
        finally
        {
            Release(sample.Crop);
            Release(sample.Original);
        }
    }
}
