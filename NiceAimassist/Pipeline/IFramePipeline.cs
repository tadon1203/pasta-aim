using NiceAimassist.Capture;

namespace NiceAimassist.Pipeline;

public interface IFramePipeline : IDisposable
{
    int InputWidth { get; }

    int InputHeight { get; }

    /// <summary>
    /// Waits a bounded time for a new desktop frame. If a frame arrives, runs the model on it.
    /// <see cref="FrameResult.Target"/> is the detection nearest to the input center. Its score is at least
    /// <paramref name="confidence"/> (0..1). The value is null when no detection qualifies.
    /// The result is <see cref="CaptureStatus.Ready"/> only when <paramref name="result"/> is filled.
    /// </summary>
    CaptureStatus TryRun(float confidence, out FrameResult result);
}
