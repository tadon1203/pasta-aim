namespace NiceAimassist.Collect;

/// <summary>Decides when the next training image is due. A stall delays the next image; it never causes a burst.</summary>
public sealed class CollectSchedule
{
    private readonly Func<long> _now;
    private readonly long _ticksPerSecond;
    private long _interval;
    private long _next;

    public CollectSchedule(Func<long> now, long ticksPerSecond)
    {
        _now = now;
        _ticksPerSecond = ticksPerSecond;
        _interval = ticksPerSecond;
    }

    public void SetRate(int perSecond) => _interval = _ticksPerSecond / Math.Max(1, perSecond);

    public void Restart() => _next = _now();

    public bool IsDue => _now() >= _next;

    public void MarkTaken() => _next = _now() + _interval;
}
