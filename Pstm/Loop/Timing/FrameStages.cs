using System.Diagnostics;
using Pstm.Detect;

namespace Pstm.Loop.Timing;

/// <summary>
/// Stage durations in Stopwatch ticks. <paramref name="Total"/> runs from the desktop timestamp to the end of the move.
/// <paramref name="Breakdown"/> splits the detect stage.
/// </summary>
public readonly record struct FrameStages(
    long Capture,
    long Detect,
    long Correction,
    long Move,
    long Total,
    DetectTimings Breakdown = default)
{
    public FrameStages Plus(in FrameStages other) => new(
        Capture + other.Capture,
        Detect + other.Detect,
        Correction + other.Correction,
        Move + other.Move,
        Total + other.Total,
        Breakdown.Plus(other.Breakdown));

    public FrameStages MaxWith(in FrameStages other) => new(
        Math.Max(Capture, other.Capture),
        Math.Max(Detect, other.Detect),
        Math.Max(Correction, other.Correction),
        Math.Max(Move, other.Move),
        Math.Max(Total, other.Total),
        Breakdown.MaxWith(other.Breakdown));

    public FrameStages Divide(long count) => count == 0
        ? default
        : new(
            Capture / count,
            Detect / count,
            Correction / count,
            Move / count,
            Total / count,
            Breakdown.Divide(count));

    public string FormatMs() =>
        $"{ToMs(Total):F2} ms (capture {ToMs(Capture):F2}, detect {ToMs(Detect):F2}, "
        + $"tracker {ToMs(Correction):F2}, move {ToMs(Move):F2})";

    /// <summary>One stage as "average/max" in milliseconds. The log writes this format.</summary>
    public static string PairMs(long average, long max) => $"{ToMs(average):F2}/{ToMs(max):F2}";

    public static double ToMs(long ticks) => ticks * 1000.0 / Stopwatch.Frequency;
}
