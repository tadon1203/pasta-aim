namespace NiceAimassist.Detect;

/// <summary>
/// One processed frame. All times are Stopwatch ticks. <paramref name="TopScore"/> is the highest finite
/// score of any output row. The box and the confidence do not matter. It is 0 when no row has a finite score.
/// </summary>
/// <param name="DesktopTimestamp">When the desktop image was last updated.</param>
/// <param name="DetectStartTimestamp">When the detector got the frame.</param>
/// <param name="DetectedTimestamp">When the model output was read and decoded.</param>
/// <param name="Breakdown">The detect stage in parts.</param>
public readonly record struct DetectedFrame(
    long DesktopTimestamp,
    long DetectStartTimestamp,
    long DetectedTimestamp,
    Detection? Target,
    float TopScore,
    DetectTimings Breakdown);
