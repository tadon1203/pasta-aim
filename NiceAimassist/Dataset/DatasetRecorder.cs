using System.Buffers;
using NiceAimassist.Capture;
using NiceAimassist.Dataset.Staging;
using NiceAimassist.Dataset.Storage;
using NiceAimassist.Shared.Gpu;

namespace NiceAimassist.Dataset;

/// <summary>
/// Copies the desktop into staging textures, maps them, and hands them to the writer thread.
/// Every method except <see cref="Status"/> and the writer callbacks runs on the loop thread. The loop thread never waits on the GPU.
/// </summary>
public sealed unsafe class DatasetRecorder : IDatasetControl, IDisposable
{
    private readonly DatasetState _state;
    private readonly StagingCopy _copy;
    private readonly SampleQueue _queue;
    private readonly DatasetWriter _dataset;
    private string _pendingFileName = string.Empty;
    private bool _disposed;

    public DatasetRecorder(GpuDevices gpu, string datasetRoot, Func<long> now, long ticksPerSecond)
    {
        _dataset = new DatasetWriter(datasetRoot);
        _state = new DatasetState(now, ticksPerSecond);
        _copy = new StagingCopy(gpu);
        _queue = new SampleQueue(Write, error => _state.ReportWriteError(error.Message));
    }

    // While the staging textures are mapped, a new GPU copy would write memory that the writer still reads.
    public bool IsDue => _state.IsDue && _copy.CanCopy;

    public DatasetStatus Status => new(_state.Active, _queue.Saved, _state.LastError);

    public void Update(DatasetSettings settings, bool held)
    {
        _state.SetEnabled(held && settings.Enabled);
        if (!_state.Active)
        {
            _copy.DropPending();
        }

        _copy.ReleaseIfCopied();
        SubmitReadySample();
    }

    public void Record(IFrameSource frames)
    {
        var desktop = frames.DesktopTexture;
        if (desktop == null)
        {
            return;
        }

        try
        {
            _copy.Copy(desktop);
            _pendingFileName = DatasetWriter.FileNameFor(DateTime.Now);
            _state.MarkRecorded();
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
        if (!_copy.IsPending)
        {
            return;
        }

        try
        {
            if (!_copy.TryMap(out MappedPixels original))
            {
                return;
            }

            bool accepted = false;
            try
            {
                accepted = _queue.TrySubmit(new Sample(_pendingFileName, original));
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
        BgraImage original = default;
        try
        {
            try
            {
                original = sample.Original.CopyToPooled();
            }
            finally
            {
                _copy.MarkCopied();
            }

            _dataset.Write(sample.FileNameWithoutExtension, original);
        }
        finally
        {
            if (original.Pixels is not null)
            {
                Release(original);
            }
        }
    }
}
