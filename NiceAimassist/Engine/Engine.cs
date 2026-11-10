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
    private readonly Func<bool>? _isCollectDown;
    private readonly AimController _aim = new();
    private bool _collectWasDown;
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
        ICollectControl? collector = null,
        Func<bool>? isCollectDown = null)
    {
        _collector = collector;
        _isCollectDown = isCollectDown;
        _pipeline = pipeline ?? throw new ArgumentNullException(nameof(pipeline));
        _sink = sink ?? throw new ArgumentNullException(nameof(sink));
        _isHeld = isHeld ?? throw new ArgumentNullException(nameof(isHeld));
        _currentSettings = currentSettings ?? throw new ArgumentNullException(nameof(currentSettings));
        _now = now ?? throw new ArgumentNullException(nameof(now));
        _onFault = onFault ?? throw new ArgumentNullException(nameof(onFault));
        _diagnostics = new EngineDiagnostics(_now);
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
            UpdateCollector(settings);
            if (!_isHeld())
            {
                if (_wasHeld)
                {
                    Log.Debug("hold key released");
                    _wasHeld = false;
                }

                _stats.SetHeld(false);
                _aim.Reset();
                _pipeline.TryCollect();
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
            if (!_pipeline.TryRun(FrameTimeoutMs, settings.Confidence / 100f, out long presentQpc, out Detection? target))
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
            bool aimed = _aim.TryUpdate(target, presentQpc, _now(), _pipeline.InputWidth, _pipeline.InputHeight, parameters, out short dx, out short dy);
            bool sent = aimed && _running;
            if (sent)
            {
                _sink.Move(dx, dy);
            }

            long latency = _now() - presentQpc;
            _stats.RecordFrame(latency);
            _diagnostics.Frame(target, aimed, sent, dx, dy, latency);
        }
    }

    private void UpdateCollector(Settings settings)
    {
        if (_collector is null)
        {
            return;
        }

        bool down = _isCollectDown?.Invoke() ?? false;
        if (!settings.CollectEnabled)
        {
            if (_collector.Active)
            {
                _collector.Toggle();
                Log.Info("collection stopped: collect_enabled is off");
            }
        }
        else if (down && !_collectWasDown)
        {
            _collector.Toggle();
        }

        _collectWasDown = down;
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
