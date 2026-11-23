namespace Pstm.Loop.Timing;

/// <summary>The Stopwatch timestamps of one frame's stages, in order.</summary>
public readonly record struct FrameTimings(
    long DesktopTimestamp,
    long DetectStartTimestamp,
    long DetectedTimestamp,
    long AimedTimestamp,
    long MovedTimestamp)
{
    public FrameStages Stages => new(
        DetectStartTimestamp - DesktopTimestamp,
        DetectedTimestamp - DetectStartTimestamp,
        AimedTimestamp - DetectedTimestamp,
        MovedTimestamp - AimedTimestamp,
        MovedTimestamp - DesktopTimestamp);
}
