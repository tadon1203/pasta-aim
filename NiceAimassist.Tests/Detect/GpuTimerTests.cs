using NiceAimassist.Detect;

namespace NiceAimassist.Tests.Detect;

public class GpuTimerTests
{
    [Fact]
    public void GpuTicksConvertToStopwatchTicksAndReversedPairIsZero()
    {
        // A 1 GHz GPU clock and a 1 MHz Stopwatch: 1000 GPU ticks are 1 microsecond, which is 1 Stopwatch tick.
        Assert.Equal(1, GpuTimer.ToStopwatchTicks(500, 1500, 1_000_000_000, 1_000_000));

        // The product of the delta and the Stopwatch frequency is above 64 bits. The result stays exact.
        Assert.Equal(40_000_000_000L, GpuTimer.ToStopwatchTicks(0, 40_000_000_000, 1_000_000_000, 1_000_000_000));

        Assert.Equal(0, GpuTimer.ToStopwatchTicks(1500, 500, 1_000_000_000, 1_000_000));
    }
}
