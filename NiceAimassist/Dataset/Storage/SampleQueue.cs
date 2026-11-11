using System.Collections.Concurrent;

namespace NiceAimassist.Dataset.Storage;

/// <summary>
/// Hands samples to a writer thread through a bounded queue. A full queue rejects the sample.
/// It does not block the caller. The caller is the loop thread.
/// </summary>
internal sealed class SampleQueue : IDisposable
{
    public const int QueueCapacity = 2;

    private readonly BlockingCollection<Sample> _queue = new(QueueCapacity);
    private readonly Action<Sample> _write;
    private readonly Action<Exception> _onError;
    private readonly Thread _thread;
    private long _saved;

    /// <param name="write">Runs on the writer thread. It reads the mapped memory of the sample only while it runs.</param>
    /// <param name="onError">Called on the writer thread when <paramref name="write"/> throws. The thread keeps running.</param>
    public SampleQueue(Action<Sample> write, Action<Exception> onError)
    {
        _write = write;
        _onError = onError;
        _thread = new Thread(Run) { Name = "dataset-writer", IsBackground = true };
        _thread.Start();
    }

    public long Saved => Volatile.Read(ref _saved);

    public bool TrySubmit(Sample sample) => _queue.TryAdd(sample);

    /// <summary>Finishes the queued samples and ends the thread.</summary>
    public void Dispose()
    {
        _queue.CompleteAdding();
        _thread.Join();
        _queue.Dispose();
    }

    private void Run()
    {
        foreach (Sample sample in _queue.GetConsumingEnumerable())
        {
            try
            {
                _write(sample);
                Interlocked.Increment(ref _saved);
            }
            catch (Exception error)
            {
                _onError(error);
            }
        }
    }
}

internal sealed record Sample(string FileNameWithoutExtension, MappedPixels Crop, MappedPixels Original);
