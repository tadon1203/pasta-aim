namespace NiceAimassist.Capture;

/// <summary>
/// Identifies one captured frame. Only <see cref="DesktopCapture"/> creates a CapturedFrame. The preprocessor
/// accepts only the newest CapturedFrame. Therefore, a frame cannot be submitted out of order.
/// </summary>
internal readonly record struct CapturedFrame(ulong Sequence, long DesktopTimestamp);
