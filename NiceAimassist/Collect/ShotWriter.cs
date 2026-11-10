using System.Collections.Concurrent;

namespace NiceAimassist.Collect;

/// <summary>
/// Hands shots to a writer thread through a bounded queue. A full queue rejects the shot instead of
/// blocking the caller, which is the engine thread.
/// </summary>
public sealed class ShotWriter : IDisposable
{
    public const int QueueCapacity = 2;

    private readonly BlockingCollection<Shot> _queue = new(QueueCapacity);
    private readonly Action<Shot> _sink;
    private readonly Action<Exception> _onError;
    private readonly Thread _thread;
    private long _saved;

    /// <param name="sink">Runs on the writer thread. It owns the shot's buffers only while it runs.</param>
    /// <param name="onError">Called on the writer thread when <paramref name="sink"/> throws. The thread keeps running.</param>
    public ShotWriter(Action<Shot> sink, Action<Exception> onError)
    {
        _sink = sink;
        _onError = onError;
        _thread = new Thread(Run) { Name = "collect-writer", IsBackground = true };
        _thread.Start();
    }

    public long Saved => Volatile.Read(ref _saved);

    public bool TrySubmit(Shot shot) => _queue.TryAdd(shot);

    /// <summary>Finishes the queued shots and ends the thread.</summary>
    public void Dispose()
    {
        _queue.CompleteAdding();
        _thread.Join();
        _queue.Dispose();
    }

    private void Run()
    {
        foreach (Shot shot in _queue.GetConsumingEnumerable())
        {
            try
            {
                _sink(shot);
                Interlocked.Increment(ref _saved);
            }
            catch (Exception error)
            {
                _onError(error);
            }
        }
    }
}

public sealed record Shot(string Stem, BgraImage Crop, BgraImage Original);
