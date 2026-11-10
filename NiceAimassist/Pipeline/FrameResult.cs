namespace NiceAimassist.Pipeline;

/// <summary>
/// One processed frame. All times are Stopwatch ticks. <paramref name="ModelBestScore"/> is the highest finite
/// score of any output row, whatever its box or confidence; it is 0 when no row has a finite score.
/// </summary>
/// <param name="PresentTimestamp">When the desktop frame was presented.</param>
/// <param name="AcquiredTimestamp">When the pipeline got the frame.</param>
/// <param name="InferredTimestamp">When the model output was read and decoded.</param>
public readonly record struct FrameResult(
    long PresentTimestamp,
    long AcquiredTimestamp,
    long InferredTimestamp,
    Detection? Target,
    float ModelBestScore);
