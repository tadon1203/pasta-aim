using System.Buffers;
using Pstm.Capture;
using Pstm.Dataset.Staging;
using Pstm.Dataset.Storage;
using Pstm.Shared.Gpu;

namespace Pstm.Dataset;

/// <summary>
/// Copies the desktop into staging textures, maps them, and hands them to the writer thread.
/// Every method except <see cref="Status"/> and the writer callbacks runs on the loop thread. The loop thread never waits on the GPU.
/// </summary>
public sealed unsafe class DatasetRecorder : IDatasetRecorder, IDisposable
{
    private readonly RecordingState _state;
    private readonly StagingTexture _copy;
    private readonly SampleWriter _queue;
    private readonly JpegWriter _dataset;
    private string _pendingFileName = string.Empty;
    private bool _disposed;

    public DatasetRecorder(GpuDevices gpu, string datasetRoot, Func<long> now, long ticksPerSecond)
    {
        _dataset = new JpegWriter(datasetRoot);
        _state = new RecordingState(now, ticksPerSecond);
        _copy = new StagingTexture(gpu);
        _queue = new SampleWriter(Write, error => _state.ReportWriteError(error.Message));
    }

    // While the staging textures are mapped, a new GPU copy would write memory that the writer still reads.
    public DatasetStatus Status => new(_state.Active, _queue.Saved, _state.LastError);

    public void ApplyEnabled(bool enabled)
    {
        _state.SetEnabled(enabled);
        if (!_state.Active)
        {
            _copy.DropPending();
        }
    }

    public void AdvancePipeline()
    {
        _copy.ReleaseIfCopied();
        SubmitReadySample();
    }

    public bool TryRecord(IFrameSource frames)
    {
        if (!_state.IsDue || !_copy.CanCopy)
        {
            return false;
        }

        var desktop = frames.DesktopTexture;
        if (desktop == null)
        {
            return false;
        }

        try
        {
            _copy.Copy(desktop);
            _pendingFileName = JpegWriter.FileNameFor(DateTime.Now);
            _state.MarkRecorded();
            return true;
        }
        catch (Exception error)
        {
            _state.Fail(error.Message);
            return false;
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
