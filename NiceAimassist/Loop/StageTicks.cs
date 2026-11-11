using System.Diagnostics;
using NiceAimassist.Detect;

namespace NiceAimassist.Loop;

/// <summary>
/// Stage durations in Stopwatch ticks. <paramref name="Total"/> runs from the desktop timestamp to the end of the move.
/// <paramref name="Submit"/> to <paramref name="Gpu"/> split the detect stage.
/// </summary>
public readonly record struct StageTicks(
    long Capture,
    long Detect,
    long Aim,
    long Move,
    long Total,
    long Submit = 0,
    long Run = 0,
    long Wait = 0,
    long Decode = 0,
    long Gpu = 0)
{
    public StageTicks WithBreakdown(in DetectBreakdown parts) =>
        this with { Submit = parts.Submit, Run = parts.Run, Wait = parts.Wait, Decode = parts.Decode, Gpu = parts.Gpu };

    public StageTicks Plus(in StageTicks other) => new(
        Capture + other.Capture,
        Detect + other.Detect,
        Aim + other.Aim,
        Move + other.Move,
        Total + other.Total,
        Submit + other.Submit,
        Run + other.Run,
        Wait + other.Wait,
        Decode + other.Decode,
        Gpu + other.Gpu);

    public StageTicks MaxWith(in StageTicks other) => new(
        Math.Max(Capture, other.Capture),
        Math.Max(Detect, other.Detect),
        Math.Max(Aim, other.Aim),
        Math.Max(Move, other.Move),
        Math.Max(Total, other.Total),
        Math.Max(Submit, other.Submit),
        Math.Max(Run, other.Run),
        Math.Max(Wait, other.Wait),
        Math.Max(Decode, other.Decode),
        Math.Max(Gpu, other.Gpu));

    public StageTicks Divide(long count) => count == 0
        ? default
        : new(
            Capture / count,
            Detect / count,
            Aim / count,
            Move / count,
            Total / count,
            Submit / count,
            Run / count,
            Wait / count,
            Decode / count,
            Gpu / count);

    /// <summary>The total and each stage in milliseconds. The status line shows these values.</summary>
    public string FormatMs() =>
        $"{ToMs(Total):F2} ms (capture {ToMs(Capture):F2}, detect {ToMs(Detect):F2}, "
        + $"aim {ToMs(Aim):F2}, move {ToMs(Move):F2})";

    /// <summary>One stage as "average/max" in milliseconds. The log writes this format.</summary>
    public static string PairMs(long average, long max) => $"{ToMs(average):F2}/{ToMs(max):F2}";

    public static double ToMs(long ticks) => ticks * 1000.0 / Stopwatch.Frequency;
}

/// <summary>The average and the worst frame of one window.</summary>
public sealed record LatencySnapshot(StageTicks Average, StageTicks Max)
{
    public static LatencySnapshot Empty { get; } = new(default, default);
}
