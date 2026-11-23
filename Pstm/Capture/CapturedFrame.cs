namespace Pstm.Capture;

/// <summary>
/// Identifies one captured frame. <see cref="DesktopCapture"/> numbers the frames. The GPU reader accepts
/// only the newest frame, and only once. Therefore, a frame cannot be read out of order.
/// </summary>
public readonly record struct CapturedFrame(ulong Sequence, long DesktopTimestamp);
