using Silk.NET.Direct3D11;

namespace NiceAimassist.Gpu;

/// <summary>Receives the held desktop frame on the engine thread, for example to collect training shots.</summary>
public interface IFrameTap
{
    /// <summary>True when the next frame should be passed to <see cref="Capture"/>.</summary>
    bool IsDue { get; }

    /// <summary>Queues the copies of the frame. <paramref name="desktop"/> must stay valid until this returns.</summary>
    unsafe void Capture(ID3D11Texture2D* desktop);
}
