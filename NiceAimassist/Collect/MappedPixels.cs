using System.Buffers;
using System.Runtime.InteropServices;

namespace NiceAimassist.Collect;

/// <summary>Mapped staging memory. It stays valid until the texture is unmapped.</summary>
public readonly record struct MappedPixels(nint Data, int RowPitch, int Width, int Height)
{
    /// <summary>Copies the rows into a pooled array. The row pitch can be longer than one row.</summary>
    public BgraImage CopyToPooled()
    {
        int rowBytes = Width * 4;
        byte[] pixels = ArrayPool<byte>.Shared.Rent(rowBytes * Height);
        try
        {
            for (int row = 0; row < Height; row++)
            {
                Marshal.Copy(Data + (nint)row * RowPitch, pixels, row * rowBytes, rowBytes);
            }
        }
        catch
        {
            ArrayPool<byte>.Shared.Return(pixels);
            throw;
        }

        return new BgraImage(pixels, Width, Height);
    }
}
