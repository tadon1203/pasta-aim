namespace NiceAimassist.Collect;

// The loop thread uses every member except ReportWriteError. The writer thread calls ReportWriteError only.
internal sealed class CollectState
{
    private readonly Func<long> _now;
    private readonly long _ticksPerSecond;
    private long _interval;
    private long _next;
    private volatile bool _active;
    private volatile string? _lastError;
    private bool _failed;
    private bool _pending;

    public CollectState(Func<long> now, long ticksPerSecond)
    {
        _now = now;
        _ticksPerSecond = ticksPerSecond;
        _interval = ticksPerSecond;
    }

    public bool Active => _active;

    public bool Pending => _pending;

    public string? LastError => _lastError;

    public bool IsDue => _active && !_pending && _now() >= _next;

    /// <summary>
    /// Starts or stops collection. A failure stays latched until <paramref name="enabled"/> is false.
    /// Stopping drops a copy that is not read yet.
    /// </summary>
    public void SetEnabled(bool enabled)
    {
        if (!enabled)
        {
            _active = false;
            _pending = false;
            _failed = false;
            return;
        }

        if (_active || _failed)
        {
            return;
        }

        _lastError = null;
        _next = _now();
        _active = true;
    }

    public void SetRate(int perSecond) => _interval = _ticksPerSecond / Math.Max(1, perSecond);

    public void MarkPending()
    {
        _pending = true;
        // A stall delays the next image. It never causes a burst.
        _next = _now() + _interval;
    }

    public void ClearPending() => _pending = false;

    /// <summary>Stops collection and keeps the message until collection is disabled.</summary>
    public void Fail(string message)
    {
        _lastError = message;
        _active = false;
        _pending = false;
        _failed = true;
    }

    /// <summary>Keeps the message. Collection continues.</summary>
    public void ReportWriteError(string message) => _lastError = message;
}
