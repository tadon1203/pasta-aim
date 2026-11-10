using System.Diagnostics;
using NiceAimassist.Aim;
using NiceAimassist.Pipeline;

namespace NiceAimassist.Engine;

/// <summary>
/// Counts what the held loop did and, once per second, logs it and publishes the window's latency to
/// <see cref="EngineStats"/>, so that "no movement" can be traced to its stage. This is the only owner of the window.
/// </summary>
internal sealed class EngineDiagnostics(Func<long> now, EngineStats stats)
{
    private readonly LatencyWindow _latency = new();
    private long _windowStart = now();
    private int _frames;
    private int _timeouts;
    private int _noFrames;
    private int _targets;
    private int _aimed;
    private int _sent;
    private float _bestScore;
    private float _modelBestScore;

    public void RecordTimeout()
    {
        _timeouts++;
        Tick();
    }

    /// <summary>Counts a wait that returned a frame without a new image, or a lost duplication.</summary>
    public void RecordNoFrame()
    {
        _noFrames++;
        Tick();
    }

    public void RecordFrame(in FrameResult frame, in FrameTimings timings, AimResult aim, bool sent)
    {
        _frames++;
        _latency.Add(timings);
        _modelBestScore = MathF.Max(_modelBestScore, frame.ModelBestScore);
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
                ? $"box=({t.X1:F1},{t.Y1:F1},{t.X2:F1},{t.Y2:F1}) score={t.Score:F3}"
                : "no target";
            Log.Trace($"frame {box} aim={aim.Outcome} move=({aim.Dx},{aim.Dy}) sent={sent}");
        }

        Tick();
    }

    /// <summary>Starts a new window, so that idle time is not counted as a slow second.</summary>
    public void Restart()
    {
        _windowStart = now();
        _frames = _timeouts = _noFrames = _targets = _aimed = _sent = 0;
        _bestScore = 0;
        _modelBestScore = 0;
        _latency.Reset();
    }

    private void Tick()
    {
        long elapsed = now() - _windowStart;
        if (elapsed < Stopwatch.Frequency)
        {
            return;
        }

        LatencySnapshot latency = _latency.Snapshot();
        if (_frames > 0)
        {
            stats.SetLatency(latency);
        }

        if (Log.IsEnabled(LogLevel.Debug))
        {
            double seconds = elapsed / (double)Stopwatch.Frequency;
            StageTicks avg = latency.Average;
            StageTicks max = latency.Max;
            Log.Debug(
                $"engine {seconds:F1}s: frames={_frames} timeouts={_timeouts} no_frame={_noFrames} with_target={_targets} "
                + $"aimed={_aimed} sent={_sent} best_score={_bestScore:F3} best_score_any_row={_modelBestScore:F3}");
            Log.Debug(
                $"latency avg/max ms: capture={StageTicks.PairMs(avg.Capture, max.Capture)} "
                + $"infer={StageTicks.PairMs(avg.Infer, max.Infer)} "
                + $"aim={StageTicks.PairMs(avg.Aim, max.Aim)} "
                + $"send={StageTicks.PairMs(avg.Send, max.Send)} "
                + $"total={StageTicks.PairMs(avg.Total, max.Total)}");
        }

        Restart();
    }
}
