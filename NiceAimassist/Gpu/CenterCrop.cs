namespace NiceAimassist.Gpu;

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
}
