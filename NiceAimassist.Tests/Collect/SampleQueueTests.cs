using NiceAimassist.Collect;

namespace NiceAimassist.Tests.Collect;

public sealed class SampleQueueTests
{
    [Fact]
    public void A_full_queue_rejects_a_sample_without_blocking()
    {
        using ManualResetEventSlim release = new();
        using ManualResetEventSlim started = new();
        using SampleQueue queue = new(_ => { started.Set(); release.Wait(); }, _ => { });
        Sample sample = new("s", Solid(1, 1), Solid(1, 1));

        Assert.True(queue.TrySubmit(sample));
        Assert.True(started.Wait(TimeSpan.FromSeconds(5)));
        for (int queued = 0; queued < SampleQueue.QueueCapacity; queued++)
        {
            Assert.True(queue.TrySubmit(sample));
        }

        Assert.False(queue.TrySubmit(sample));
        release.Set();
    }

    private static BgraImage Solid(int width, int height) => new(new byte[width * height * 4], width, height);
}
