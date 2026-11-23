using System.Diagnostics;
using NiceAimassist.Aim;
using NiceAimassist.Loop;
using NiceAimassist.Loop.Timing;
using NiceAimassist.Detect;

namespace NiceAimassist.Tests.Loop;

public class LoopDiagnosticsTests
{
    [Fact]
    public void StageDurationsAreDifferencesOfConsecutiveTimestamps()
    {
        FrameStages stages = new FrameTimings(100, 110, 150, 155, 170).Stages;

        Assert.Equal(new FrameStages(10, 40, 5, 15, 70), stages);
    }

    [Fact]
    public void WindowReportsAverageAndMaxPerStage()
    {
        FrameStages first = new(10, 20, 0, 0, 0, new DetectTimings(Submit: 4, Gpu: 8));
        FrameStages second = new(30, 5, 0, 0, 0, new DetectTimings(Submit: 2, Gpu: 12));

        FrameStages average = first.Plus(second).Divide(2);
        FrameStages max = first.MaxWith(second);

        Assert.Equal(new FrameStages(20, 12, 0, 0, 0, new DetectTimings(Submit: 3, Gpu: 10)), average);
        Assert.Equal(new FrameStages(30, 20, 0, 0, 0, new DetectTimings(Submit: 4, Gpu: 12)), max);
    }

    [Fact]
    public void LatencyIsPublishedOnlyWhenAFullSecondHadFrames()
    {
        long clock = 0;
        LoopStatus status = new();
        LoopDiagnostics diagnostics = new(() => clock, status.SetLatency);
        DetectedFrame frame = new(0, 0, 0, null, 0f, default);
        AimResult move = new(AimOutcome.Move, 0, 0);

        diagnostics.RecordFrame(frame, new FrameTimings(0, 1, 2, 3, 4), new System.Drawing.Size(320, 320), move, true);
        Assert.Same(FrameStats.Empty, status.Latency);

        clock = Stopwatch.Frequency;
        diagnostics.RecordFrame(frame, new FrameTimings(0, 1, 2, 3, 4), new System.Drawing.Size(320, 320), move, true);
        Assert.Equal(4, status.Latency.Average.Total);

        // A window with only timeouts keeps the last published value.
        clock += Stopwatch.Frequency;
        diagnostics.RecordTimeout();
        Assert.Equal(4, status.Latency.Average.Total);
    }
}
