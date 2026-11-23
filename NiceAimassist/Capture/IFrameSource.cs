using Silk.NET.Direct3D11;

namespace NiceAimassist.Capture;

/// <summary>Gives the loop the desktop frames, one at a time. The loop thread uses it.</summary>
public interface IFrameSource
{
    /// <summary>
    /// Waits a bounded time for a new desktop frame. The result is <see cref="CaptureOutcome.Ready"/>
    /// only when <paramref name="frame"/> is filled. The previous frame ends with this call.
    /// </summary>
    CaptureOutcome TryAcquire(int timeoutMs, out CapturedFrame frame);

    /// <summary>Ends the held frame now. The next <see cref="TryAcquire"/> then waits for a new frame.</summary>
    void ReleaseHeldFrame();

    /// <summary>The desktop image of the held frame. It is null before the first frame. It is invalid after the next <see cref="TryAcquire"/>.</summary>
    unsafe ID3D11Texture2D* DesktopTexture { get; }
}
