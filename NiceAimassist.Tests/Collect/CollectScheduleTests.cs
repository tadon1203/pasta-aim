using NiceAimassist.Collect;

namespace NiceAimassist.Tests.Collect;

public sealed class CollectScheduleTests
{
    [Fact]
    public void ScheduleDoesNotBurstAfterAStall()
    {
        long now = 0;
        CollectSchedule schedule = new(() => now, ticksPerSecond: 1000);
        schedule.SetRate(2);
        schedule.Restart();

        Assert.True(schedule.IsDue);
        schedule.MarkTaken();
        Assert.False(schedule.IsDue);

        now = 10_000;
        Assert.True(schedule.IsDue);
        schedule.MarkTaken();
        Assert.False(schedule.IsDue);

        now += 499;
        Assert.False(schedule.IsDue);
        now += 1;
        Assert.True(schedule.IsDue);
    }
}
