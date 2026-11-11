namespace NiceAimassist.Loop;

/// <summary>Frame statistics. The loop thread writes them. Any thread can read them.</summary>
public sealed class LoopStats
{
    private long _frames;
    private volatile LatencySnapshot _latency = LatencySnapshot.Empty;
    private volatile bool _held;
    private volatile bool _collecting;
    private volatile string? _collectError;
    private long _saved;

    public long Frames => Volatile.Read(ref _frames);

    /// <summary>The latency of the last full one-second window that had frames. The numbers are the same as in the log.</summary>
    public LatencySnapshot Latency => _latency;

    public bool Held => _held;

    public bool Collecting => _collecting;

    public long Saved => Volatile.Read(ref _saved);

    public string? CollectError => _collectError;

    internal void SetCollect(bool collecting, long saved, string? error)
    {
        _collecting = collecting;
        Volatile.Write(ref _saved, saved);
        _collectError = error;
    }

    internal void RecordFrame() => Interlocked.Increment(ref _frames);

    internal void SetLatency(LatencySnapshot latency) => _latency = latency;

    internal void SetHeld(bool held) => _held = held;
}
