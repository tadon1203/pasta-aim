namespace NiceAimassist.Capture;

public enum CaptureOutcome
{
    /// <summary>A new frame was copied and can be submitted.</summary>
    Ready,

    /// <summary>No frame was presented within the wait.</summary>
    Timeout,

    /// <summary>The duplication returned a frame without a new image.</summary>
    Unchanged,

    /// <summary>The duplication was lost and recreated. The next wait continues on the new duplication.</summary>
    Lost,
}
