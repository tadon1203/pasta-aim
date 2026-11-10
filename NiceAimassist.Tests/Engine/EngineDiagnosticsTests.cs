using System.Diagnostics;
using NiceAimassist.Engine;
using NiceAimassist.Pipeline;

namespace NiceAimassist.Tests.Engine;

public class EngineDiagnosticsTests
{
    [Fact]
    public void StageDurationsAreDifferencesOfConsecutiveTimestamps()
    {
        StageTicks stages = new FrameTimings(100, 110, 150, 155, 170).Stages;

        Assert.Equal(new StageTicks(10, 40, 5, 15, 70), stages);
    }

    [Fact]
    public void WindowReportsAverageAndMaxPerStage()
    {
        LatencyWindow window = new();
        window.Add(new FrameTimings(0, 10, 20, 30, 40));
        window.Add(new FrameTimings(0, 30, 80, 90, 100));

        LatencySnapshot snapshot = window.Snapshot();

        Assert.Equal(new StageTicks(20, 30, 10, 10, 70), snapshot.Average);
        Assert.Equal(new StageTicks(30, 50, 10, 10, 100), snapshot.Max);
    }

    [Fact]
    public void LatencyIsPublishedOnlyWhenAFullSecondHadFrames()
    {
        long clock = 0;
        EngineStats stats = new();
        EngineDiagnostics diagnostics = new(() => clock, stats);
        FrameResult frame = new(0, 0, 0, null, 0f);

        diagnostics.Frame(frame, new FrameTimings(0, 1, 2, 3, 4), true, true, 0, 0);
        Assert.Same(LatencySnapshot.Empty, stats.Latency);

        clock = Stopwatch.Frequency;
        diagnostics.Frame(frame, new FrameTimings(0, 1, 2, 3, 4), true, true, 0, 0);
        Assert.Equal(4, stats.Latency.Average.Total);

        // A window with only timeouts keeps the last published value.
        clock += Stopwatch.Frequency;
        diagnostics.Timeout();
        Assert.Equal(4, stats.Latency.Average.Total);
    }
}
