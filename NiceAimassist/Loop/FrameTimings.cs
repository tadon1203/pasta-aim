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
}
