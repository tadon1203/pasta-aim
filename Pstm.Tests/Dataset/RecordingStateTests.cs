using Pstm.Dataset;

namespace Pstm.Tests.Dataset;

public sealed class RecordingStateTests
{
    private static RecordingState NewState(Func<long> now) => new(now, ticksPerSecond: 1000);

    [Fact]
    public void A_failure_stays_latched_until_recording_is_disabled()
    {
        RecordingState state = NewState(() => 0);
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
    public void Enabling_again_makes_the_next_image_due_at_once()
    {
        RecordingState state = NewState(() => 0);
        state.SetEnabled(true);
        state.MarkRecorded();
        Assert.False(state.IsDue);

        state.SetEnabled(false);
        Assert.False(state.IsDue);

        state.SetEnabled(true);
        Assert.True(state.IsDue);
    }

    [Fact]
    public void A_write_error_is_kept_and_recording_continues()
    {
        RecordingState state = NewState(() => 0);
        state.SetEnabled(true);

        state.ReportWriteError("the disk is full");

        Assert.True(state.Active);
        Assert.Equal("the disk is full", state.LastError);
    }

    [Fact]
    public void An_image_is_due_two_seconds_after_the_last_one_and_a_stall_causes_no_burst()
    {
        long now = 0;
        RecordingState state = new(() => now, ticksPerSecond: 1000);
        state.SetEnabled(true);

        Assert.True(state.IsDue);
        state.MarkRecorded();
        Assert.False(state.IsDue);

        now = 10_000;
        Assert.True(state.IsDue);
        state.MarkRecorded();
        Assert.False(state.IsDue);

        now += 1_999;
        Assert.False(state.IsDue);
        now += 1;
        Assert.True(state.IsDue);
    }
}
