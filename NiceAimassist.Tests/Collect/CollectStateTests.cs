using NiceAimassist.Collect;

namespace NiceAimassist.Tests.Collect;

public sealed class CollectStateTests
{
    private static CollectState NewState()
    {
        var schedule = new CollectSchedule(() => 0, ticksPerSecond: 1000);
        schedule.SetRate(2);
        return new CollectState(schedule);
    }

    [Fact]
    public void A_failure_stays_latched_until_collection_is_disabled()
    {
        CollectState state = NewState();
        state.SetEnabled(true);
        state.Fail("the duplication stopped");

        state.SetEnabled(true);
        Assert.False(state.Active);
        Assert.Equal("the duplication stopped", state.LastError);

        state.SetEnabled(false);
        state.SetEnabled(true);
        Assert.True(state.Active);
        Assert.Null(state.LastError);
    }

    [Fact]
    public void Disabling_clears_a_pending_copy()
    {
        CollectState state = NewState();
        state.SetEnabled(true);
        state.MarkPending();
        Assert.False(state.IsDue);

        state.SetEnabled(false);

        Assert.False(state.Pending);
        state.SetEnabled(true);
        Assert.True(state.IsDue);
    }

    [Fact]
    public void A_write_error_is_kept_and_collection_continues()
    {
        CollectState state = NewState();
        state.SetEnabled(true);

        state.ReportWriteError("the disk is full");

        Assert.True(state.Active);
        Assert.Equal("the disk is full", state.LastError);
    }
}
