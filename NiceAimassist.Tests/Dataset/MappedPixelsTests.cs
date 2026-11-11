using System.Buffers;
using System.Runtime.InteropServices;
using NiceAimassist.Dataset;

namespace NiceAimassist.Tests.Dataset;

public sealed class MappedPixelsTests : IDisposable
{
    private const int Width = 3;
    private const int Height = 2;
    private const int RowPitch = 16;
    private const byte Padding = 0xEE;

    private readonly nint _memory = Marshal.AllocHGlobal(RowPitch * Height);

    public void Dispose() => Marshal.FreeHGlobal(_memory);

    [Fact]
    public void CopyToPooled_skips_the_row_padding()
    {
        for (int row = 0; row < Height; row++)
        {
            for (int column = 0; column < RowPitch; column++)
            {
                byte value = column < Width * 4 ? (byte)(row * 50 + column) : Padding;
                Marshal.WriteByte(_memory, row * RowPitch + column, value);
            }
        }

        BgraImage image = new MappedPixels(_memory, RowPitch, Width, Height).CopyToPooled();

        try
        {
            Assert.Equal((Width, Height), (image.Width, image.Height));
            for (int row = 0; row < Height; row++)
            {
                for (int column = 0; column < Width * 4; column++)
                {
                    Assert.Equal((byte)(row * 50 + column), image.Pixels[row * Width * 4 + column]);
                }
            }
        }
        finally
        {
            ArrayPool<byte>.Shared.Return(image.Pixels);
        }
    }
}
