using System.Drawing;
using NiceAimassist.Collect;

namespace NiceAimassist.Tests.Collect;

public class CollectTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), "nice-aimassist-collect-" + Guid.NewGuid().ToString("N"));

    public void Dispose()
    {
        if (Directory.Exists(_root))
        {
            Directory.Delete(_root, recursive: true);
        }
    }

    [Theory]
    [InlineData(1920, 1080, 640, 640)]
    [InlineData(1920, 1080, 641, 641)]
    [InlineData(1920, 1080, 4096, 1080)]
    public void RoiIsACenteredSquareThatFitsTheDesktop(int dw, int dh, int size, int expectedSide)
    {
        (int left, int top, int side) = CollectRoi.Square(dw, dh, size);

        Assert.Equal(expectedSide, side);
        Assert.Equal((dw - side) / 2, left);
        Assert.Equal((dh - side) / 2, top);
        Assert.True(left >= 0 && top >= 0 && left + side <= dw && top + side <= dh);
    }

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

    [Fact]
    public void WriterSavesACropAndAnOriginalAsJpeg()
    {
        DatasetWriter writer = new(_root);

        writer.Write("shot", Solid(64, 48), Solid(160, 90));

        string crop = Path.Combine(_root, "images", "train", "shot.jpg");
        string original = Path.Combine(_root, "original", "shot.jpg");
        Assert.Equal([(byte)0xFF, (byte)0xD8], File.ReadAllBytes(crop).Take(2).ToArray());
        using (Image image = Image.FromFile(crop))
        {
            Assert.Equal((64, 48), (image.Width, image.Height));
        }

        using (Image image = Image.FromFile(original))
        {
            Assert.Equal((160, 90), (image.Width, image.Height));
        }

        Assert.Empty(Directory.GetFiles(_root, "*.tmp", SearchOption.AllDirectories));
    }

    [Fact]
    public void FullQueueRejectsAShotWithoutBlocking()
    {
        using ManualResetEventSlim release = new();
        using ManualResetEventSlim started = new();
        using ShotWriter writer = new(_ => { started.Set(); release.Wait(); }, _ => { });
        Shot shot = new("s", Solid(1, 1), Solid(1, 1));

        Assert.True(writer.TrySubmit(shot));
        Assert.True(started.Wait(TimeSpan.FromSeconds(5)));
        for (int queued = 0; queued < ShotWriter.QueueCapacity; queued++)
        {
            Assert.True(writer.TrySubmit(shot));
        }

        Assert.False(writer.TrySubmit(shot));
        release.Set();
    }

    private static BgraImage Solid(int width, int height) => new(new byte[width * height * 4], width, height);
}
