namespace Pstm.Loop.Timing;

/// <summary>The Stopwatch timestamps of one frame's stages, in order.</summary>
public readonly record struct FrameTimings(
    long DesktopTimestamp,
    long DetectStartTimestamp,
    long DetectedTimestamp,
    long CorrectionedTimestamp,
    long MovedTimestamp)
{
    public FrameStages Stages => new(
        DetectStartTimestamp - DesktopTimestamp,
        DetectedTimestamp - DetectStartTimestamp,
        CorrectionedTimestamp - DetectedTimestamp,
        MovedTimestamp - CorrectionedTimestamp,
        MovedTimestamp - DesktopTimestamp);
}
