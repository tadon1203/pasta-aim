using D3D11Box = Silk.NET.Direct3D11.Box;

namespace NiceAimassist.Capture;

public static class CenterCrop
{
    public static (int Left, int Top) Origin(int desktopWidth, int desktopHeight, int inputWidth, int inputHeight)
    {
        if (inputWidth > desktopWidth || inputHeight > desktopHeight)
        {
            throw new ArgumentException(
                $"Model input {inputWidth}x{inputHeight} is larger than the captured desktop {desktopWidth}x{desktopHeight}.");
        }

        return ((desktopWidth - inputWidth) / 2, (desktopHeight - inputHeight) / 2);
    }

    /// <summary>The source box of a centered <paramref name="width"/> x <paramref name="height"/> region of the desktop.</summary>
    internal static D3D11Box Box(int desktopWidth, int desktopHeight, int width, int height)
    {
        (int left, int top) = Origin(desktopWidth, desktopHeight, width, height);
        return new D3D11Box
        {
            Left = (uint)left,
            Top = (uint)top,
            Front = 0,
            Right = (uint)(left + width),
            Bottom = (uint)(top + height),
            Back = 1,
        };
    }
}
