namespace NiceAimassist.Gpu;

/// <summary>
/// Names one captured frame. Only <see cref="FrameSource"/> creates tickets, and the preprocessor accepts
/// only the newest one, so a frame cannot be submitted out of order.
/// </summary>
internal readonly record struct FrameTicket(ulong Sequence, long PresentTimestamp);
