namespace Pstm.Loop.Timing;

/// <summary>The average and the worst frame of one window.</summary>
public sealed record FrameStats(FrameStages Average, FrameStages Max)
{
    public static FrameStats Empty { get; } = new(default, default);
}
