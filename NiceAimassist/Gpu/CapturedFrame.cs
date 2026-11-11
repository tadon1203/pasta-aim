namespace NiceAimassist.Gpu;

/// <summary>
/// Identifies one captured frame. Only <see cref="FrameSource"/> creates a CapturedFrame. The preprocessor
/// accepts only the newest CapturedFrame. Therefore, a frame cannot be submitted out of order.
/// </summary>
internal readonly record struct CapturedFrame(ulong Sequence, long DesktopTimestamp);
