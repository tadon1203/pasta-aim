using NiceAimassist.Gpu;

namespace NiceAimassist.Tests.Gpu;

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
}
