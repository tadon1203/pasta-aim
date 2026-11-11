using System.Drawing;
using NiceAimassist.Aim;
using NiceAimassist.Collect;
using NiceAimassist.Config;
using NiceAimassist.Capture;
using NiceAimassist.Detect;
using NiceAimassist.Mouse;

namespace NiceAimassist.Loop;

/// <summary>Runs the aim loop on one dedicated thread.</summary>
public sealed class AimLoop
{
    private const int IdleSleepMs = 1;

    private readonly IDetector _detector;
    private readonly IMouse _mouse;
    private readonly Func<bool> _isHeld;
    private readonly Func<Settings> _currentSettings;
    private readonly Func<long> _now;
    private readonly Action<Exception> _onFault;
    private readonly ICollectControl? _collector;
    private readonly AimController _aim = new();
    private bool _wasHeld;
    private readonly LoopDiagnostics _diagnostics;
    private readonly LoopStats _stats = new();
    private volatile bool _running;
    private Thread? _thread;
    private bool _interpolationSent;
    private Interpolation _sentInterpolation;

    /// <param name="now">Source of the Stopwatch timestamps. Usually <c>Stopwatch.GetTimestamp</c>.</param>
    /// <param name="onFault">Called on the loop thread when the detector or the mouse throws. The loop ends after the call.</param>
    public AimLoop(
        IDetector detector,
        IMouse mouse,
        Func<bool> isHeld,
        Func<Settings> currentSettings,
        Func<long> now,
        Action<Exception> onFault,
        ICollectControl? collector = null)
    {
        _collector = collector;
        _detector = detector ?? throw new ArgumentNullException(nameof(detector));
        _mouse = mouse ?? throw new ArgumentNullException(nameof(mouse));
        _isHeld = isHeld ?? throw new ArgumentNullException(nameof(isHeld));
        _currentSettings = currentSettings ?? throw new ArgumentNullException(nameof(currentSettings));
        _now = now ?? throw new ArgumentNullException(nameof(now));
        _onFault = onFault ?? throw new ArgumentNullException(nameof(onFault));
        _diagnostics = new LoopDiagnostics(_now, _stats);
    }

    public LoopStats Stats => _stats;

    public void Start()
    {
        if (_thread is not null)
        {
            throw new InvalidOperationException("the loop is already started");
        }

        _running = true;
        _thread = new Thread(ThreadMain) { Name = "loop", IsBackground = true };
        _thread.Start();
    }

    /// <summary>Stops the loop and waits for the loop thread to exit. A frame in progress ends without a mouse move.</summary>
    public void Stop()
    {
        _running = false;
        Thread? thread = _thread;
        if (thread is not null && thread != Thread.CurrentThread)
        {
            thread.Join();
        }
    }

    private void ThreadMain()
    {
        try
        {
            Loop();
        }
        catch (Exception error)
        {
            Log.Error($"the loop faulted: {error}");
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
                Log.Debug($"hold key pressed; confidence={settings.Detect.Confidence}% input={_detector.InputWidth}x{_detector.InputHeight}");
                _diagnostics.Restart();
                _wasHeld = true;
            }

            _stats.SetHeld(true);
            SendInterpolation(settings.Mouse.Interpolation);
            CaptureStatus status = _detector.TryDetect(settings.Detect.Confidence / 100f, out DetectedFrame frame);
            if (status == CaptureStatus.Timeout)
            {
                _diagnostics.RecordTimeout();
                continue;
            }

            if (status != CaptureStatus.Ready)
            {
                _diagnostics.RecordNoFrame();
                continue;
            }

            AimResult aim = _aim.Update(
                frame.Target,
                frame.DesktopTimestamp,
                _now(),
                new Size(_detector.InputWidth, _detector.InputHeight),
                settings.Aim);
            long aimedAt = _now();
            // Read the flag after the aim. A stop during the frame then suppresses its move.
            bool sent = aim.Status == AimStatus.Move && _running;
            if (sent)
            {
                _mouse.Move(aim.Dx, aim.Dy);
            }

            long sentAt = sent ? _now() : aimedAt;
            FrameTimings timings = new(frame.DesktopTimestamp, frame.CapturedTimestamp, frame.DetectedTimestamp, aimedAt, sentAt);
            _stats.RecordFrame();
            _diagnostics.RecordFrame(frame, timings, aim, sent);
        }
    }

    // Collection runs only while the hold key is down. It uses the same frames as the aim.
    private void UpdateCollector(Settings settings, bool held)
    {
        if (_collector is null)
        {
            return;
        }

        _collector.SetEnabled(held && settings.CollectEnabled);
        _collector.Configure(settings.CollectSize, settings.CollectRate);
        _collector.SubmitReadySample();
        _stats.SetCollect(_collector.Active, _collector.Saved, _collector.LastError);
    }

    private void SendInterpolation(Interpolation interpolation)
    {
        if (_interpolationSent && interpolation == _sentInterpolation)
        {
            return;
        }

        _mouse.SetInterpolation(interpolation);
        Log.Debug($"interpolation set to {(interpolation.FixedPercent is { } percent ? percent + "%" : "auto")}");
        _sentInterpolation = interpolation;
        _interpolationSent = true;
    }
}
