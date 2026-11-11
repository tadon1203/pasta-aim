using System.Collections.Concurrent;

namespace NiceAimassist.Collect;

/// <summary>
/// Hands samples to a writer thread through a bounded queue. A full queue rejects the sample.
/// It does not block the caller. The caller is the loop thread.
/// </summary>
public sealed class SampleWriter : IDisposable
{
    public const int QueueCapacity = 2;

    private readonly BlockingCollection<Sample> _queue = new(QueueCapacity);
    private readonly Action<Sample> _sink;
    private readonly Action<Exception> _onError;
    private readonly Thread _thread;
    private long _saved;

    /// <param name="sink">Runs on the writer thread. It owns the buffers of the sample only while it runs.</param>
    /// <param name="onError">Called on the writer thread when <paramref name="sink"/> throws. The thread keeps running.</param>
    public SampleWriter(Action<Sample> sink, Action<Exception> onError)
    {
        _sink = sink;
        _onError = onError;
        _thread = new Thread(Run) { Name = "collect-writer", IsBackground = true };
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
                _sink(sample);
                Interlocked.Increment(ref _saved);
            }
            catch (Exception error)
            {
                _onError(error);
            }
        }
    }
}

public sealed record Sample(string FileNameWithoutExtension, BgraImage Crop, BgraImage Original);
