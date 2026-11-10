using NiceAimassist.Aim;
using NiceAimassist.Collect;
using NiceAimassist.Config;
using NiceAimassist.Pipeline;

namespace NiceAimassist.Engine;

/// <summary>Runs the aim loop on one dedicated thread.</summary>
public sealed class Engine
{
    private const int FrameTimeoutMs = 100;
    private const int IdleSleepMs = 1;

    private readonly IFramePipeline _pipeline;
    private readonly IMoveSink _sink;
    private readonly Func<bool> _isHeld;
    private readonly Func<Settings> _currentSettings;
    private readonly Func<long> _now;
    private readonly Action<Exception> _onFault;
    private readonly ICollectControl? _collector;
    private readonly AimController _aim = new();
    private bool _wasHeld;
    private readonly EngineDiagnostics _diagnostics;
    private readonly EngineStats _stats = new();
    private volatile bool _running;
    private Thread? _thread;
    private bool _interpolationSent;
    private Interpolation _sentInterpolation;

    /// <param name="now">Stopwatch timestamp source, normally <c>Stopwatch.GetTimestamp</c>.</param>
    /// <param name="onFault">Called on the engine thread when the pipeline or the sink throws. The loop ends after it.</param>
    public Engine(
        IFramePipeline pipeline,
        IMoveSink sink,
        Func<bool> isHeld,
        Func<Settings> currentSettings,
        Func<long> now,
        Action<Exception> onFault,
        ICollectControl? collector = null)
    {
        _collector = collector;
        _pipeline = pipeline ?? throw new ArgumentNullException(nameof(pipeline));
        _sink = sink ?? throw new ArgumentNullException(nameof(sink));
        _isHeld = isHeld ?? throw new ArgumentNullException(nameof(isHeld));
        _currentSettings = currentSettings ?? throw new ArgumentNullException(nameof(currentSettings));
        _now = now ?? throw new ArgumentNullException(nameof(now));
        _onFault = onFault ?? throw new ArgumentNullException(nameof(onFault));
        _diagnostics = new EngineDiagnostics(_now, _stats);
    }

    public EngineStats Stats => _stats;

    public void Start()
    {
        if (_thread is not null)
        {
            throw new InvalidOperationException("the engine is already started");
        }

        _running = true;
        _thread = new Thread(Run) { Name = "engine", IsBackground = true };
        _thread.Start();
    }

    /// <summary>Stops the loop and waits for the engine thread to exit. A frame in progress finishes without moving the mouse.</summary>
    public void Stop()
    {
        _running = false;
        Thread? thread = _thread;
        if (thread is not null && thread != Thread.CurrentThread)
        {
            thread.Join();
        }
    }

    private void Run()
    {
        try
        {
            Loop();
        }
        catch (Exception error)
        {
            Log.Error($"the engine faulted: {error}");
            _onFault(error);
        }
    }

    private void Loop()
    {
        while (_running)
        {
            Settings settings = _currentSettings();
            bool held = _isHeld();
            UpdateCollector(settings, held);
            if (!held)
            {
                if (_wasHeld)
                {
                    Log.Debug("hold key released");
                    _wasHeld = false;
                }

                _stats.SetHeld(false);
                _aim.Reset();
                Thread.Sleep(IdleSleepMs);
                continue;
            }

            if (!_wasHeld)
            {
                Log.Debug($"hold key pressed; confidence={settings.Confidence}% input={_pipeline.InputWidth}x{_pipeline.InputHeight}");
                _diagnostics.Restart();
                _wasHeld = true;
            }

            _stats.SetHeld(true);
            SendInterpolation(settings.Interpolation);
            if (!_pipeline.TryRun(FrameTimeoutMs, settings.Confidence / 100f, out FrameResult frame))
            {
                _diagnostics.Timeout();
                continue;
            }

            AimParameters parameters = new(
                settings.SensitivityX,
                settings.SensitivityY,
                settings.TargetYOffset,
                settings.MaxDelta,
                settings.VelocitySmoothing);
            // The flag is read after the aim so that a stop during the frame suppresses its move.
            bool aimed = _aim.TryUpdate(frame.Target, frame.PresentQpc, _now(), _pipeline.InputWidth, _pipeline.InputHeight, parameters, out short dx, out short dy);
            long aimedAt = _now();
            bool sent = aimed && _running;
            if (sent)
            {
                _sink.Move(dx, dy);
            }

            long sentAt = sent ? _now() : aimedAt;
            FrameTimings timings = new(frame.PresentQpc, frame.AcquiredQpc, frame.InferredQpc, aimedAt, sentAt);
            _stats.RecordFrame();
            _diagnostics.Frame(frame, timings, aimed, sent, dx, dy);
        }
    }

    // Collection runs only while the hold key is down, so it follows the same frames the aim uses.
    private void UpdateCollector(Settings settings, bool held)
    {
        if (_collector is null)
        {
            return;
        }

        _collector.Update(held && settings.CollectEnabled);
        _collector.Configure(settings.CollectSize, settings.CollectRate);
        _collector.Poll();
        _stats.SetCollect(_collector.Active, _collector.Saved, _collector.LastError);
    }

    private void SendInterpolation(Interpolation interpolation)
    {
        if (_interpolationSent && interpolation == _sentInterpolation)
        {
            return;
        }

        _sink.SetInterpolation(interpolation);
        Log.Debug($"interpolation set to {(interpolation.FixedPercent is { } percent ? percent + "%" : "auto")}");
        _sentInterpolation = interpolation;
        _interpolationSent = true;
    }
}
