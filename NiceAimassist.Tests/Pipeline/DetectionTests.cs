using NiceAimassist.Pipeline;

namespace NiceAimassist.Tests.Pipeline;

public class DetectionTests
{
    [Fact]
    public void IoUOfIdenticalBoxesIsOne()
    {
        Detection box = new(0, 0, 10, 10, 0.9f, 0);

        Assert.Equal(1f, Detection.IoU(box, box));
    }

    [Fact]
    public void IoUOfDisjointBoxesIsZero()
    {
        Detection a = new(0, 0, 10, 10, 0.9f, 0);
        Detection b = new(20, 0, 30, 10, 0.9f, 0);

        Assert.Equal(0f, Detection.IoU(a, b));
    }

    [Fact]
    public void IoUOfBoxesShiftedByHalfTheirWidthIsOneThird()
    {
        // The shared area is 50. The union is 150.
        Detection a = new(0, 0, 10, 10, 0.9f, 0);
        Detection b = new(5, 0, 15, 10, 0.9f, 0);

        Assert.True(MathF.Abs(Detection.IoU(a, b) - 1f / 3f) < 1e-6f);
    }
}
