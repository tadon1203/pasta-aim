namespace NiceAimassist.Loop;

/// <summary>Accumulates frame timings. It has no clock. The caller decides where a window ends.</summary>
internal sealed class LatencyWindow
{
    private long _capture;
    private long _infer;
    private long _aim;
    private long _send;
    private long _total;
    private StageTicks _max;

    public int Count { get; private set; }

    public void Add(in FrameTimings timings)
    {
        StageTicks stages = timings.Stages;
        _capture += stages.Capture;
        _infer += stages.Infer;
        _aim += stages.Aim;
        _send += stages.Send;
        _total += stages.Total;
        _max = new StageTicks(
            Math.Max(_max.Capture, stages.Capture),
            Math.Max(_max.Infer, stages.Infer),
            Math.Max(_max.Aim, stages.Aim),
            Math.Max(_max.Send, stages.Send),
            Math.Max(_max.Total, stages.Total));
        Count++;
    }

    public LatencySnapshot Snapshot()
    {
        if (Count == 0)
        {
            return LatencySnapshot.Empty;
        }

        return new LatencySnapshot(
            new StageTicks(_capture / Count, _infer / Count, _aim / Count, _send / Count, _total / Count),
            _max);
    }

    public void Reset()
    {
        _capture = _infer = _aim = _send = _total = 0;
        _max = default;
        Count = 0;
    }
}
