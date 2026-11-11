using Silk.NET.Direct3D11;

namespace NiceAimassist.Capture;

/// <summary>Receives the held desktop frame on the loop thread. Collection uses it to save training samples.</summary>
public interface IFrameObserver
{
    /// <summary>True when the next frame should be passed to <see cref="Observe"/>.</summary>
    bool IsDue { get; }

    /// <summary>Queues the copies of the frame. <paramref name="desktop"/> must stay valid until this method returns.</summary>
    unsafe void Observe(ID3D11Texture2D* desktop);
}
