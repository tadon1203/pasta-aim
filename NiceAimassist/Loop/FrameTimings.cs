using System.Diagnostics;

namespace NiceAimassist.Loop;

/// <summary>The Stopwatch timestamps of one frame's stages, in order.</summary>
public readonly record struct FrameTimings(
    long DesktopTimestamp,
    long CapturedTimestamp,
    long DetectedTimestamp,
    long AimedTimestamp,
    long MovedTimestamp)
{
    public StageTicks Stages => new(
        CapturedTimestamp - DesktopTimestamp,
        DetectedTimestamp - CapturedTimestamp,
        AimedTimestamp - DetectedTimestamp,
        MovedTimestamp - AimedTimestamp,
        MovedTimestamp - DesktopTimestamp);

    public static double ToMs(long ticks) => ticks * 1000.0 / Stopwatch.Frequency;
}

/// <summary>Stage durations in Stopwatch ticks. <paramref name="Total"/> runs from the desktop timestamp to the end of the move.</summary>
public readonly record struct StageTicks(long Capture, long Detect, long Aim, long Move, long Total)
{
    /// <summary>The total and each stage in milliseconds. The status line shows these values.</summary>
    public string FormatMs() =>
        $"{FrameTimings.ToMs(Total):F2} ms (capture {FrameTimings.ToMs(Capture):F2}, detect {FrameTimings.ToMs(Detect):F2}, "
        + $"aim {FrameTimings.ToMs(Aim):F2}, move {FrameTimings.ToMs(Move):F2})";

    /// <summary>One stage as "average/max" in milliseconds. The log writes this format.</summary>
    public static string PairMs(long average, long max) => $"{FrameTimings.ToMs(average):F2}/{FrameTimings.ToMs(max):F2}";
}

/// <summary>The average and the worst frame of one window.</summary>
public sealed record LatencySnapshot(StageTicks Average, StageTicks Max)
{
    public static LatencySnapshot Empty { get; } = new(default, default);
}
