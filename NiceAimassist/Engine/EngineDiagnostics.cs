using System.Diagnostics;
using NiceAimassist.Pipeline;

namespace NiceAimassist.Engine;

/// <summary>Counts what the held loop did and logs it once per second, so that "no movement" can be traced to its stage.</summary>
internal sealed class EngineDiagnostics(Func<long> now)
{
    private long _windowStart = now();
    private int _frames;
    private int _timeouts;
    private int _targets;
    private int _aimed;
    private int _sent;
    private float _bestScore;
    private double _latencyTicks;

    public void Timeout()
    {
        _timeouts++;
        Tick();
    }

    public void Frame(Detection? target, bool aimed, bool sent, short dx, short dy, long latencyTicks)
    {
        _frames++;
        _latencyTicks += latencyTicks;
        if (target is { } detection)
        {
            _targets++;
            _bestScore = MathF.Max(_bestScore, detection.Score);
        }

        _aimed += aimed ? 1 : 0;
        _sent += sent ? 1 : 0;

        if (Log.IsEnabled(LogLevel.Trace))
        {
            string box = target is { } t
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
        _latencyTicks = 0;
    }

    private void Tick()
    {
        long elapsed = now() - _windowStart;
        if (elapsed < Stopwatch.Frequency)
        {
            return;
        }

        if (Log.IsEnabled(LogLevel.Debug))
        {
            double seconds = elapsed / (double)Stopwatch.Frequency;
            double latencyMs = _frames == 0 ? 0 : _latencyTicks / _frames * 1000.0 / Stopwatch.Frequency;
            Log.Debug(
                $"engine {seconds:F1}s: frames={_frames} timeouts={_timeouts} with_target={_targets} "
                + $"aimed={_aimed} sent={_sent} best_score={_bestScore:F3} latency={latencyMs:F2}ms");
        }

        Restart();
    }
}
