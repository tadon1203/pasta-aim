namespace NiceAimassist.Pipeline;

public interface IFramePipeline : IDisposable
{
    int InputWidth { get; }

    int InputHeight { get; }

    /// <summary>
    /// Waits up to <paramref name="timeoutMs"/> for a new desktop frame and runs the model on it.
    /// Returns false on timeout. <paramref name="presentQpc"/> is the frame's present time in
    /// Stopwatch ticks; <paramref name="target"/> is the detection nearest to the input center whose
    /// score is at least <paramref name="confidence"/> (0..1), or null.
    /// </summary>
    bool TryRun(int timeoutMs, float confidence, out long presentQpc, out Detection? target);

    /// <summary>
    /// Passes a new desktop frame to the training-data collector when one is due, without running the model.
    /// Returns false when nothing was collected. Does not wait for a frame.
    /// </summary>
    bool TryCollect() => false;
}
