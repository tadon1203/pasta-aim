namespace NiceAimassist.Gpu;

/// <summary>The outcome of waiting for one desktop frame.</summary>
public enum AcquireResult
{
    /// <summary>A new frame was copied and can be submitted.</summary>
    Frame,

    /// <summary>No frame was presented within the wait.</summary>
    Timeout,

    /// <summary>The duplication returned a frame without a new image.</summary>
    NoNewImage,

    /// <summary>The duplication was lost and recreated; the next wait continues on the new duplication.</summary>
    DeviceLost,
}
