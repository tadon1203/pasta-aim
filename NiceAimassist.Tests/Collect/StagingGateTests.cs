using NiceAimassist.Collect;

namespace NiceAimassist.Tests.Collect;

public sealed class StagingGateTests
{
    [Fact]
    public void A_copy_is_refused_while_the_textures_are_mapped()
    {
        StagingGate gate = new();
        Assert.True(gate.CanCopy);

        gate.Map();

        Assert.False(gate.CanCopy);
    }

    [Fact]
    public void The_textures_stay_mapped_until_the_writer_has_copied_them()
    {
        StagingGate gate = new();
        gate.Map();

        Assert.Throws<InvalidOperationException>(gate.Release);
        Assert.False(gate.CanCopy);

        gate.MarkCopied();
        gate.Release();
        Assert.True(gate.CanCopy);
    }

    [Fact]
    public void A_sample_the_queue_rejects_frees_the_textures_at_once()
    {
        StagingGate gate = new();
        gate.Map();

        gate.CancelMap();

        Assert.True(gate.CanCopy);
    }
}
