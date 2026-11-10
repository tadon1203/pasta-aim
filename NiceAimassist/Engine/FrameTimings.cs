using System.Diagnostics;

namespace NiceAimassist.Engine;

/// <summary>The Stopwatch timestamps of one frame's stages, in order.</summary>
public readonly record struct FrameTimings(long Present, long Acquired, long Inferred, long Aimed, long Sent)
{
    public StageTicks Stages => new(Acquired - Present, Inferred - Acquired, Aimed - Inferred, Sent - Aimed, Sent - Present);

    public static double ToMs(long ticks) => ticks * 1000.0 / Stopwatch.Frequency;
}

/// <summary>Stage durations in Stopwatch ticks. <paramref name="Total"/> runs from the frame's present to the end of the send.</summary>
public readonly record struct StageTicks(long Capture, long Infer, long Aim, long Send, long Total);

/// <summary>The average and the worst frame of one window.</summary>
public sealed record LatencySnapshot(StageTicks Average, StageTicks Max)
{
    public static LatencySnapshot Empty { get; } = new(default, default);
}
