using Silk.NET.Direct3D11;

namespace NiceAimassist.Detect;

/// <summary>Queues GPU copies of the held desktop frame for training data. It runs on the loop thread.</summary>
public interface IFrameCollector
{
    /// <summary>True when the next frame should be passed to <see cref="Collect"/>.</summary>
    bool IsDue { get; }

    /// <summary>Queues the copies of the frame. <paramref name="desktop"/> must stay valid until this method returns.</summary>
    unsafe void Collect(ID3D11Texture2D* desktop);
}
