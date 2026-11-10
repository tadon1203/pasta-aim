namespace NiceAimassist.Pipeline;

/// <summary>
/// One processed frame. All times are Stopwatch ticks. <paramref name="ModelBestScore"/> is the highest score
/// of any output row and is measured only while debug logging is on; otherwise it is 0.
/// </summary>
/// <param name="PresentQpc">When the desktop frame was presented.</param>
/// <param name="AcquiredQpc">When the pipeline got the frame.</param>
/// <param name="InferredQpc">When the model output was read and decoded.</param>
public readonly record struct FrameResult(
    long PresentQpc,
    long AcquiredQpc,
    long InferredQpc,
    Detection? Target,
    float ModelBestScore);
