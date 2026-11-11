using NiceAimassist.Capture;

namespace NiceAimassist.Tests.Capture;

public class CenterCropTests
{
    [Fact]
    public void CropIsCenteredOnTheDesktop()
    {
        Assert.Equal((420, 240), CenterCrop.Origin(1920, 1080, 1080, 600));
    }

    [Fact]
    public void OddDifferenceRoundsTheOriginDown()
    {
        Assert.Equal((1, 0), CenterCrop.Origin(7, 4, 4, 4));
    }

    [Fact]
    public void InputLargerThanTheDesktopIsRejected()
    {
        Assert.Throws<ArgumentException>(() => CenterCrop.Origin(640, 480, 641, 480));
    }

    [Theory]
    [InlineData(1920, 1080, 640, 640)]
    [InlineData(1920, 1080, 641, 641)]
    [InlineData(1920, 1080, 4096, 1080)]
    public void SquareIsCenteredAndFitsTheDesktop(int desktopWidth, int desktopHeight, int size, int expectedSide)
    {
        (int left, int top, int side) = CenterCrop.Square(desktopWidth, desktopHeight, size);

        Assert.Equal(expectedSide, side);
        Assert.Equal(((desktopWidth - side) / 2, (desktopHeight - side) / 2), (left, top));
        Assert.True(left >= 0 && top >= 0 && left + side <= desktopWidth && top + side <= desktopHeight);
    }
}
