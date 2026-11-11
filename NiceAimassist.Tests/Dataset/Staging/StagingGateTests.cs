using NiceAimassist.Dataset.Staging;

namespace NiceAimassist.Tests.Dataset.Staging;

public sealed class StagingGateTests
{
    [Fact]
    public void A_second_copy_is_refused_while_a_copy_is_pending()
    {
        StagingGate gate = new();
        gate.BeginCopy();

        Assert.False(gate.CanCopy);
        Assert.True(gate.IsPending);
        Assert.Throws<InvalidOperationException>(gate.BeginCopy);
    }

    [Fact]
    public void A_pending_copy_that_nobody_maps_can_be_dropped()
    {
        StagingGate gate = new();
        gate.BeginCopy();

        gate.CancelCopy();

        Assert.True(gate.CanCopy);
        Assert.Throws<InvalidOperationException>(gate.CancelCopy);
    }

    [Fact]
    public void The_textures_can_be_mapped_only_after_a_copy()
    {
        StagingGate gate = new();
        Assert.Throws<InvalidOperationException>(gate.Map);

        gate.BeginCopy();
        gate.Map();

        Assert.False(gate.CanCopy);
        Assert.False(gate.IsPending);
        Assert.True(gate.IsMapped);
    }

    [Fact]
    public void The_textures_stay_mapped_until_the_writer_has_copied_them()
    {
        StagingGate gate = new();
        gate.BeginCopy();
        gate.Map();

        Assert.Throws<InvalidOperationException>(gate.Release);
        Assert.False(gate.CanCopy);

        gate.MarkCopied();
        Assert.True(gate.IsMapped);
        gate.Release();
        Assert.True(gate.CanCopy);
        Assert.False(gate.IsMapped);
    }

    [Fact]
    public void A_sample_the_queue_rejects_frees_the_textures_at_once()
    {
        StagingGate gate = new();
        gate.BeginCopy();
        gate.Map();

        gate.CancelMap();

        Assert.True(gate.CanCopy);
    }
}
