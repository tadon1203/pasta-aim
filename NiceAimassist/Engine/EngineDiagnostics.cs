using System.Diagnostics;
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
    private int _targets;
    private int _aimed;
    private int _sent;
    private float _bestScore;
    private float _modelBestScore;

    public void Timeout()
    {
        _timeouts++;
        Tick();
    }

    public void Frame(in FrameResult frame, in FrameTimings timings, bool aimed, bool sent, short dx, short dy)
    {
        _frames++;
        _latency.Add(timings);
        _modelBestScore = MathF.Max(_modelBestScore, frame.ModelBestScore);
        if (frame.Target is { } detection)
        {
            _targets++;
            _bestScore = MathF.Max(_bestScore, detection.Score);
        }

        _aimed += aimed ? 1 : 0;
        _sent += sent ? 1 : 0;

        if (Log.IsEnabled(LogLevel.Trace))
        {
            string box = frame.Target is { } t
                ? $"box=({t.X1:F1},{t.Y1:F1},{t.X2:F1},{t.Y2:F1}) score={t.Score:F3}"
                : "no target";
            Log.Trace($"frame {box} move=({dx},{dy}) aimed={aimed} sent={sent}");
        }

        Tick();
    }

    /// <summary>Starts a new window, so that idle time is not counted as a slow second.</summary>
    public void Restart()
    {
        _windowStart = now();
        _frames = _timeouts = _targets = _aimed = _sent = 0;
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
                $"engine {seconds:F1}s: frames={_frames} timeouts={_timeouts} with_target={_targets} "
                + $"aimed={_aimed} sent={_sent} best_score={_bestScore:F3} best_score_any_row={_modelBestScore:F3}");
            Log.Debug(
                $"latency avg/max ms: capture={FrameTimings.ToMs(avg.Capture):F2}/{FrameTimings.ToMs(max.Capture):F2} "
                + $"infer={FrameTimings.ToMs(avg.Infer):F2}/{FrameTimings.ToMs(max.Infer):F2} "
                + $"aim={FrameTimings.ToMs(avg.Aim):F2}/{FrameTimings.ToMs(max.Aim):F2} "
                + $"send={FrameTimings.ToMs(avg.Send):F2}/{FrameTimings.ToMs(max.Send):F2} "
                + $"total={FrameTimings.ToMs(avg.Total):F2}/{FrameTimings.ToMs(max.Total):F2}");
        }

        Restart();
    }
}
