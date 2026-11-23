using Pstm.Shared.Logging;

namespace Pstm.Tests.Shared.Logging;

public class LogWriterTests
{
    [Fact]
    public void DisposeWritesEveryPendingLineInOrder()
    {
        string directory = Path.Combine(Path.GetTempPath(), "pstm-log-" + Guid.NewGuid().ToString("N"));
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
    public void WriteAfterDisposeDropsTheLineAndDoesNotThrow()
    {
        string path = Path.Combine(Path.GetTempPath(), "pstm-log-" + Guid.NewGuid().ToString("N") + ".log");
        LogWriter writer = new(path);
        writer.Write("kept");
        writer.Dispose();

        try
        {
            writer.Write("late");

            string[] lines = File.ReadAllLines(path);
            Assert.Contains("kept", lines);
            Assert.DoesNotContain("late", lines);
        }
        finally
        {
            File.Delete(path);
        }
    }
}
