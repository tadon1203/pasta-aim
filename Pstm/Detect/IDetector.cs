using Pstm.Capture;

namespace Pstm.Detect;

public interface IDetector : IDisposable
{
    int InputWidth { get; }

    int InputHeight { get; }

    /// <summary>
    /// Runs the model on a frame that <see cref="IFrameSource.TryAcquire"/> returned. The frame must be the newest one.
    /// <see cref="DetectedFrame.Target"/> is the detection nearest to the input center. Its score is at least
    /// <paramref name="threshold"/> (0..1). The value is null when no detection qualifies.
    /// </summary>
    DetectedFrame Detect(CapturedFrame frame, float threshold);
}
