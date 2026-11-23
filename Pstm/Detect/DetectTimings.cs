namespace Pstm.Detect;

/// <summary>
/// The detect stage in parts. <paramref name="Submit"/>, <paramref name="Run"/>, <paramref name="Wait"/> and
/// <paramref name="Decode"/> are CPU time in Stopwatch ticks. <paramref name="Gpu"/> is GPU time in Stopwatch ticks.
/// </summary>
public readonly record struct DetectTimings(
    long Submit = 0,
    long Run = 0,
    long Wait = 0,
    long Decode = 0,
    long Gpu = 0)
{
    public DetectTimings Plus(in DetectTimings other) => new(
        Submit + other.Submit,
        Run + other.Run,
        Wait + other.Wait,
        Decode + other.Decode,
        Gpu + other.Gpu);

    public DetectTimings MaxWith(in DetectTimings other) => new(
        Math.Max(Submit, other.Submit),
        Math.Max(Run, other.Run),
        Math.Max(Wait, other.Wait),
        Math.Max(Decode, other.Decode),
        Math.Max(Gpu, other.Gpu));

    public DetectTimings Divide(long count) => count == 0
        ? default
        : new(Submit / count, Run / count, Wait / count, Decode / count, Gpu / count);
}
