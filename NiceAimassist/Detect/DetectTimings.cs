namespace NiceAimassist.Detect;

/// <summary>
/// The detect stage in parts. <paramref name="Submit"/>, <paramref name="Run"/>, <paramref name="Wait"/> and
/// <paramref name="Decode"/> are CPU time in Stopwatch ticks. <paramref name="Gpu"/> is GPU time in Stopwatch ticks.
/// </summary>
public readonly record struct DetectTimings(long Submit, long Run, long Wait, long Decode, long Gpu);
