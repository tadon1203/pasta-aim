using NiceAimassist.Gpu;

namespace NiceAimassist.Collect;

public static class CollectRoi
{
    /// <summary>The centered square of <paramref name="size"/> px, shrunk to fit the desktop.</summary>
    public static (int Left, int Top, int Side) Square(int desktopWidth, int desktopHeight, int size)
    {
        int side = Math.Min(size, Math.Min(desktopWidth, desktopHeight));
        (int left, int top) = CenterCrop.Origin(desktopWidth, desktopHeight, side, side);
        return (left, top, side);
    }
}
