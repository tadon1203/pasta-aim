using NiceAimassist.Dataset;
using NiceAimassist.Dataset.Storage;

namespace NiceAimassist.Tests.Dataset.Storage;

public sealed class SampleQueueTests
{
    [Fact]
    public void A_full_queue_rejects_a_sample_without_blocking()
    {
        using ManualResetEventSlim release = new();
        using ManualResetEventSlim started = new();
        using SampleQueue queue = new(_ => { started.Set(); release.Wait(); }, _ => { });
        Sample sample = new("s", Unmapped());

        Assert.True(queue.TrySubmit(sample));
        Assert.True(started.Wait(TimeSpan.FromSeconds(5)));
        for (int queued = 0; queued < SampleQueue.QueueCapacity; queued++)
        {
            Assert.True(queue.TrySubmit(sample));
        }

        Assert.False(queue.TrySubmit(sample));
        release.Set();
    }

    private static MappedPixels Unmapped() => new(nint.Zero, RowPitch: 4, Width: 1, Height: 1);
}
