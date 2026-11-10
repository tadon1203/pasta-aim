namespace NiceAimassist.Engine;

/// <summary>Frame statistics. The engine thread writes them; any thread may read them.</summary>
public sealed class EngineStats
{
    private long _frames;
    private long _lastLatencyTicks;
    private volatile bool _held;
    private volatile bool _collecting;
    private volatile string? _collectError;
    private long _saved;

    public long Frames => Volatile.Read(ref _frames);

    /// <summary>Stopwatch ticks from the last frame's present time to the end of its processing.</summary>
    public long LastLatencyTicks => Volatile.Read(ref _lastLatencyTicks);

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

    internal void RecordFrame(long latencyTicks)
    {
        Volatile.Write(ref _lastLatencyTicks, latencyTicks);
        Interlocked.Increment(ref _frames);
    }

    internal void SetHeld(bool held) => _held = held;
}
