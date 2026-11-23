namespace Pstm.Dataset;

// The loop thread uses every member except ReportWriteError. The writer thread calls ReportWriteError only.
internal sealed class RecordingState
{
    private readonly Func<long> _now;
    private const int IntervalSeconds = 2;

    private readonly long _interval;
    private long _next;
    private volatile bool _active;
    private volatile string? _lastError;
    private bool _failed;

    public RecordingState(Func<long> now, long ticksPerSecond)
    {
        _now = now;
        _interval = ticksPerSecond * IntervalSeconds;
    }

    public bool Active => _active;

    public string? LastError => _lastError;

    public bool IsDue => _active && _now() >= _next;

    /// <summary>Starts or stops recording. A failure stays latched until <paramref name="enabled"/> is false.</summary>
    public void SetEnabled(bool enabled)
    {
        if (!enabled)
        {
            _active = false;
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

    public void MarkRecorded()
    {
        // A stall delays the next image. It never causes a burst.
        _next = _now() + _interval;
    }

    /// <summary>Stops recording and keeps the message until recording is disabled.</summary>
    public void Fail(string message)
    {
        _lastError = message;
        _active = false;
        _failed = true;
    }

    /// <summary>Keeps the message. Recording continues.</summary>
    public void ReportWriteError(string message) => _lastError = message;
}
