namespace NiceAimassist.Pipeline;

/// <summary>
/// One processed frame. All times are Stopwatch ticks. <paramref name="ModelBestScore"/> is the highest finite
/// score of any output row. The box and the confidence do not matter. It is 0 when no row has a finite score.
/// </summary>
/// <param name="DesktopTimestamp">When the desktop image was last updated.</param>
/// <param name="AcquiredTimestamp">When the pipeline got the frame.</param>
/// <param name="InferredTimestamp">When the model output was read and decoded.</param>
public readonly record struct FrameResult(
    long DesktopTimestamp,
    long AcquiredTimestamp,
    long InferredTimestamp,
    Detection? Target,
    float ModelBestScore);
