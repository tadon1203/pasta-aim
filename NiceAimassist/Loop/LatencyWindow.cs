namespace NiceAimassist.Loop;

/// <summary>Accumulates frame timings. It has no clock. The caller decides where a window ends.</summary>
internal sealed class LatencyWindow
{
    private long _capture;
    private long _detect;
    private long _aim;
    private long _move;
    private long _total;
    private StageTicks _max;

    public int Count { get; private set; }

    public void Add(in FrameTimings timings)
    {
        StageTicks stages = timings.Stages;
        _capture += stages.Capture;
        _detect += stages.Detect;
        _aim += stages.Aim;
        _move += stages.Move;
        _total += stages.Total;
        _max = new StageTicks(
            Math.Max(_max.Capture, stages.Capture),
            Math.Max(_max.Detect, stages.Detect),
            Math.Max(_max.Aim, stages.Aim),
            Math.Max(_max.Move, stages.Move),
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
            new StageTicks(_capture / Count, _detect / Count, _aim / Count, _move / Count, _total / Count),
            _max);
    }

    public void Reset()
    {
        _capture = _detect = _aim = _move = _total = 0;
        _max = default;
        Count = 0;
    }
}
