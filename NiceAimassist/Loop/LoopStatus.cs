using NiceAimassist.Collect;

namespace NiceAimassist.Loop;

/// <summary>Frame statistics. The loop thread writes them. Any thread can read them.</summary>
public sealed class LoopStatus
{
    private readonly object _collectGate = new();
    private long _frames;
    private volatile LatencySnapshot _latency = LatencySnapshot.Empty;
    private volatile bool _held;
    private CollectStatus _collect;

    public long Frames => Volatile.Read(ref _frames);

    /// <summary>The latency of the last full one-second window that had frames. The numbers are the same as in the log.</summary>
    public LatencySnapshot Latency => _latency;

    public bool Held => _held;

    /// <summary>The collection state of the last loop.</summary>
    public CollectStatus Collect
    {
        get
        {
            lock (_collectGate)
            {
                return _collect;
            }
        }

        internal set
        {
            lock (_collectGate)
            {
                _collect = value;
            }
        }
    }

    internal void RecordFrame() => Interlocked.Increment(ref _frames);

    internal void SetLatency(LatencySnapshot latency) => _latency = latency;

    internal void SetHeld(bool held) => _held = held;
}
