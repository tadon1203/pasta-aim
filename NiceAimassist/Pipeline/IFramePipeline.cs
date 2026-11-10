using NiceAimassist.Gpu;

namespace NiceAimassist.Pipeline;

public interface IFramePipeline : IDisposable
{
    int InputWidth { get; }

    int InputHeight { get; }

    /// <summary>
    /// Waits a bounded time for a new desktop frame and, when there is one, runs the model on it.
    /// <see cref="FrameResult.Target"/> is the detection nearest to the input center whose score is at
    /// least <paramref name="confidence"/> (0..1), or null. The result is <see cref="AcquireResult.Frame"/>
    /// only when <paramref name="result"/> is filled.
    /// </summary>
    AcquireResult TryRun(float confidence, out FrameResult result);
}
