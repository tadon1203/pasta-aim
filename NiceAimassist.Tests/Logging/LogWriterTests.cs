using NiceAimassist.Logging;

namespace NiceAimassist.Tests.Logging;

public class LogWriterTests
{
    [Fact]
    public void DisposeWritesEveryPendingLineInOrder()
    {
        string directory = Path.Combine(Path.GetTempPath(), "nice-aimassist-log-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        string path = Path.Combine(directory, "test.log");
        try
        {
            LogWriter writer = new(path);
            for (int i = 0; i < 1000; i++)
            {
                writer.Write($"line {i}");
            }

            writer.Dispose();

            string[] lines = File.ReadAllLines(path);
            Assert.Equal(1000, lines.Length);
            Assert.Equal("line 0", lines[0]);
            Assert.Equal("line 999", lines[^1]);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void WriteAfterDisposeDoesNotThrow()
    {
        string path = Path.Combine(Path.GetTempPath(), "nice-aimassist-log-" + Guid.NewGuid().ToString("N") + ".log");
        LogWriter writer = new(path);
        writer.Dispose();

        try
        {
            writer.Write("late");
        }
        finally
        {
            File.Delete(path);
        }
    }
}
