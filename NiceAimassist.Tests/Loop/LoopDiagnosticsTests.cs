using System.Diagnostics;
using NiceAimassist.Aim;
using NiceAimassist.Loop;
using NiceAimassist.Detect;

namespace NiceAimassist.Tests.Loop;

public class LoopDiagnosticsTests
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
        StageTicks first = new(10, 20, 0, 0, 0, Submit: 4, Gpu: 8);
        StageTicks second = new(30, 5, 0, 0, 0, Submit: 2, Gpu: 12);

        StageTicks average = first.Plus(second).Divide(2);
        StageTicks max = first.MaxWith(second);

        Assert.Equal(new StageTicks(20, 12, 0, 0, 0, Submit: 3, Gpu: 10), average);
        Assert.Equal(new StageTicks(30, 20, 0, 0, 0, Submit: 4, Gpu: 12), max);
    }

    [Fact]
    public void LatencyIsPublishedOnlyWhenAFullSecondHadFrames()
    {
        long clock = 0;
        LoopStatus status = new();
        LoopDiagnostics diagnostics = new(() => clock, status.SetLatency);
        DetectedFrame frame = new(0, 0, 0, null, 0f, default);
        AimResult move = new(AimStatus.Move, 0, 0);

        diagnostics.RecordFrame(frame, new FrameTimings(0, 1, 2, 3, 4), move, true);
        Assert.Same(LatencySnapshot.Empty, status.Latency);

        clock = Stopwatch.Frequency;
        diagnostics.RecordFrame(frame, new FrameTimings(0, 1, 2, 3, 4), move, true);
        Assert.Equal(4, status.Latency.Average.Total);

        // A window with only timeouts keeps the last published value.
        clock += Stopwatch.Frequency;
        diagnostics.RecordTimeout();
        Assert.Equal(4, status.Latency.Average.Total);
    }
}
