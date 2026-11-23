using NiceAimassist.Loop.Timing;

namespace NiceAimassist.Loop;

/// <summary>Frame statistics. The loop thread writes them. Any thread can read them.</summary>
public sealed class LoopStatus
{
    private long _frames;
    private volatile FrameStats _latency = FrameStats.Empty;
    private volatile bool _held;

    public long Frames => Volatile.Read(ref _frames);

    /// <summary>The latency of the last full one-second window that had frames. The numbers are the same as in the log.</summary>
    public FrameStats Latency => _latency;

    public bool Held => _held;

    internal void RecordFrame() => Interlocked.Increment(ref _frames);

    internal void SetLatency(FrameStats latency) => _latency = latency;

    internal void SetHeld(bool held) => _held = held;
}
