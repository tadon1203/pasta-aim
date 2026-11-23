using System.Diagnostics;
using System.Drawing;
using NiceAimassist.Aim;
using NiceAimassist.Detect;
using NiceAimassist.Loop.Timing;
using NiceAimassist.Shared.Logging;

namespace NiceAimassist.Loop;

/// <summary>
/// Counts what the held loop did. Once per second, it logs the counts and passes the latency to
/// <paramref name="publishLatency"/>. A "no movement" result can then be traced to its stage.
/// </summary>
internal sealed class LoopDiagnostics(Func<long> now, Action<FrameStats> publishLatency)
{
    private FrameStages _sum;
    private FrameStages _max;
    private long _windowStart = now();
    private int _frames;
    private int _timeouts;
    private int _noFrames;
    private int _targets;
    private int _aimed;
    private int _sent;
    private float _bestScore;
    private float _topScore;
    private long _previousFrameAt;

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

    public void RecordFrame(in DetectedFrame frame, in FrameTimings timings, Size input, AimResult aim, bool sent)
    {
        _frames++;
        FrameStages stages = timings.Stages.WithBreakdown(frame.Breakdown);
        _sum = _sum.Plus(stages);
        _max = _max.MaxWith(stages);
        _topScore = MathF.Max(_topScore, frame.TopScore);
        if (frame.Target is { } detection)
        {
            _targets++;
            _bestScore = MathF.Max(_bestScore, detection.Score);
        }

        bool aimed = aim.Outcome == AimOutcome.Move;
        _aimed += aimed ? 1 : 0;
        _sent += sent ? 1 : 0;

        if (Log.IsEnabled(LogLevel.Trace))
        {
            string box = frame.Target is { } t
                ? $"box=({t.X1:F1},{t.Y1:F1},{t.X2:F1},{t.Y2:F1}) score={t.Score:F3} "
                    + $"err=({t.CenterX - input.Width * 0.5f:F1},{t.CenterY - input.Height * 0.5f:F1})"
                : "no target";
            double intervalMs = _previousFrameAt == 0
                ? 0
                : (timings.MovedTimestamp - _previousFrameAt) * 1000.0 / Stopwatch.Frequency;
            Log.Trace($"frame {box} aim={aim.Outcome} move=({aim.Dx},{aim.Dy}) sent={sent} dt={intervalMs:F1}ms");
        }

        _previousFrameAt = timings.MovedTimestamp;

        Tick();
    }

    /// <summary>Starts a new window. Idle time is then not counted as a slow second.</summary>
    public void Restart()
    {
        _windowStart = now();
        _previousFrameAt = 0;
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

        FrameStats latency = FrameStats.Empty;
        if (_frames > 0)
        {
            latency = new FrameStats(_sum.Divide(_frames), _max);
            publishLatency(latency);
        }

        if (Log.IsEnabled(LogLevel.Debug))
        {
            double seconds = elapsed / (double)Stopwatch.Frequency;
            FrameStages avg = latency.Average;
            FrameStages max = latency.Max;
            Log.Debug(
                $"loop {seconds:F1}s: frames={_frames} timeouts={_timeouts} no_frame={_noFrames} with_target={_targets} "
                + $"aimed={_aimed} sent={_sent} best_score={_bestScore:F3} top_score={_topScore:F3}");
            Log.Debug(
                $"latency avg/max ms: capture={FrameStages.PairMs(avg.Capture, max.Capture)} "
                + $"detect={FrameStages.PairMs(avg.Detect, max.Detect)} "
                + $"aim={FrameStages.PairMs(avg.Aim, max.Aim)} "
                + $"move={FrameStages.PairMs(avg.Move, max.Move)} "
                + $"total={FrameStages.PairMs(avg.Total, max.Total)}");
            Log.Debug(
                $"detect breakdown avg/max ms: submit={FrameStages.PairMs(avg.Submit, max.Submit)} "
                + $"run={FrameStages.PairMs(avg.Run, max.Run)} "
                + $"wait={FrameStages.PairMs(avg.Wait, max.Wait)} "
                + $"decode={FrameStages.PairMs(avg.Decode, max.Decode)} "
                + $"gpu={FrameStages.PairMs(avg.Gpu, max.Gpu)}");
        }

        Restart();
    }
}
