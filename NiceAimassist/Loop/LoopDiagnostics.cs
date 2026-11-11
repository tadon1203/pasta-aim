using System.Diagnostics;
using NiceAimassist.Aim;
using NiceAimassist.Detect;
using NiceAimassist.Shared.Logging;

namespace NiceAimassist.Loop;

/// <summary>
/// Counts what the held loop did. Once per second, it logs the counts and passes the latency to
/// <paramref name="publishLatency"/>. A "no movement" result can then be traced to its stage.
/// </summary>
internal sealed class LoopDiagnostics(Func<long> now, Action<LatencySnapshot> publishLatency)
{
    private StageTicks _sum;
    private StageTicks _max;
    private long _windowStart = now();
    private int _frames;
    private int _timeouts;
    private int _noFrames;
    private int _targets;
    private int _aimed;
    private int _sent;
    private float _bestScore;
    private float _topScore;

    public void RecordTimeout()
    {
        _timeouts++;
        Tick();
    }

    /// <summary>Counts a wait that returned a frame without a new image. It also counts a lost duplication.</summary>
    public void RecordNoFrame()
    {
        _noFrames++;
        Tick();
    }

    public void RecordFrame(in DetectedFrame frame, in FrameTimings timings, AimResult aim, bool sent)
    {
        _frames++;
        StageTicks stages = timings.Stages.WithBreakdown(frame.Breakdown);
        _sum = _sum.Plus(stages);
        _max = _max.MaxWith(stages);
        _topScore = MathF.Max(_topScore, frame.TopScore);
        if (frame.Target is { } detection)
        {
            _targets++;
            _bestScore = MathF.Max(_bestScore, detection.Score);
        }

        bool aimed = aim.Status == AimStatus.Move;
        _aimed += aimed ? 1 : 0;
        _sent += sent ? 1 : 0;

        if (Log.IsEnabled(LogLevel.Trace))
        {
            string box = frame.Target is { } t
                ? $"box=({t.X1:F1},{t.Y1:F1},{t.X2:F1},{t.Y2:F1}) score={t.Score:F3}"
                : "no target";
            Log.Trace($"frame {box} aim={aim.Status} move=({aim.Dx},{aim.Dy}) sent={sent}");
        }

        Tick();
    }

    /// <summary>Starts a new window. Idle time is then not counted as a slow second.</summary>
    public void Restart()
    {
        _windowStart = now();
        _frames = _timeouts = _noFrames = _targets = _aimed = _sent = 0;
        _bestScore = 0;
        _topScore = 0;
        _sum = default;
        _max = default;
    }

    private void Tick()
    {
        long elapsed = now() - _windowStart;
        if (elapsed < Stopwatch.Frequency)
        {
            return;
        }

        LatencySnapshot latency = LatencySnapshot.Empty;
        if (_frames > 0)
        {
            latency = new LatencySnapshot(_sum.Divide(_frames), _max);
            publishLatency(latency);
        }

        if (Log.IsEnabled(LogLevel.Debug))
        {
            double seconds = elapsed / (double)Stopwatch.Frequency;
            StageTicks avg = latency.Average;
            StageTicks max = latency.Max;
            Log.Debug(
                $"loop {seconds:F1}s: frames={_frames} timeouts={_timeouts} no_frame={_noFrames} with_target={_targets} "
                + $"aimed={_aimed} sent={_sent} best_score={_bestScore:F3} top_score={_topScore:F3}");
            Log.Debug(
                $"latency avg/max ms: capture={StageTicks.PairMs(avg.Capture, max.Capture)} "
                + $"detect={StageTicks.PairMs(avg.Detect, max.Detect)} "
                + $"aim={StageTicks.PairMs(avg.Aim, max.Aim)} "
                + $"move={StageTicks.PairMs(avg.Move, max.Move)} "
                + $"total={StageTicks.PairMs(avg.Total, max.Total)}");
            Log.Debug(
                $"detect breakdown avg/max ms: submit={StageTicks.PairMs(avg.Submit, max.Submit)} "
                + $"run={StageTicks.PairMs(avg.Run, max.Run)} "
                + $"wait={StageTicks.PairMs(avg.Wait, max.Wait)} "
                + $"decode={StageTicks.PairMs(avg.Decode, max.Decode)} "
                + $"gpu={StageTicks.PairMs(avg.Gpu, max.Gpu)}");
        }

        Restart();
    }
}
