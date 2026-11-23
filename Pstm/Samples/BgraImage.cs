namespace Pstm.Samples;

/// <summary>A tightly packed BGRA image. <see cref="Pixels"/> may be longer than the image.</summary>
internal readonly record struct BgraImage(byte[] Pixels, int Width, int Height);
