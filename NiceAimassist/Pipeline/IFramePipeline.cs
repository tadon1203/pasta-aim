namespace NiceAimassist.Pipeline;

public interface IFramePipeline : IDisposable
{
    int InputWidth { get; }

    int InputHeight { get; }

    /// <summary>
    /// Waits up to <paramref name="timeoutMs"/> for a new desktop frame and runs the model on it.
    /// Returns false on timeout. <see cref="FrameResult.Target"/> is the detection nearest to the input
    /// center whose score is at least <paramref name="confidence"/> (0..1), or null.
    /// </summary>
    bool TryRun(int timeoutMs, float confidence, out FrameResult result);
}
