using System.Diagnostics;

namespace NiceAimassist.Engine;

/// <summary>The Stopwatch timestamps of one frame's stages, in order.</summary>
public readonly record struct FrameTimings(long Present, long Acquired, long Inferred, long Aimed, long Sent)
{
    public StageTicks Stages => new(Acquired - Present, Inferred - Acquired, Aimed - Inferred, Sent - Aimed, Sent - Present);

    public static double ToMs(long ticks) => ticks * 1000.0 / Stopwatch.Frequency;
}

/// <summary>Stage durations in Stopwatch ticks. <paramref name="Total"/> runs from the frame's present to the end of the send.</summary>
public readonly record struct StageTicks(long Capture, long Infer, long Aim, long Send, long Total)
{
    /// <summary>The total, then each stage in milliseconds, as shown in the status line.</summary>
    public string FormatMs() =>
        $"{FrameTimings.ToMs(Total):F2} ms (capture {FrameTimings.ToMs(Capture):F2}, infer {FrameTimings.ToMs(Infer):F2}, "
        + $"aim {FrameTimings.ToMs(Aim):F2}, send {FrameTimings.ToMs(Send):F2})";

    /// <summary>One stage as "average/max" in milliseconds, as written to the log.</summary>
    public static string PairMs(long average, long max) => $"{FrameTimings.ToMs(average):F2}/{FrameTimings.ToMs(max):F2}";
}

/// <summary>The average and the worst frame of one window.</summary>
public sealed record LatencySnapshot(StageTicks Average, StageTicks Max)
{
    public static LatencySnapshot Empty { get; } = new(default, default);
}
