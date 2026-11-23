using System.Drawing;
using Pstm.Aim;
using Pstm.Dataset;
using Pstm.Settings;
using Pstm.Capture;
using Pstm.Detect;
using Pstm.Loop.Timing;
using Pstm.Shared.Logging;
using Pstm.Mouse;

namespace Pstm.Loop;

public sealed class FrameLoop
{
    private const int IdleSleepMs = 1;
    private const int AcquireTimeoutMs = 100;

    private readonly IFrameSource _frames;
    private readonly IDetector _detector;
    private readonly IMouse _mouse;
    private readonly Func<bool> _isHeld;
    private readonly Func<AppSettings> _currentSettings;
    private readonly Func<long> _now;
    private readonly Action<Exception> _onFault;
    private readonly IDatasetRecorder? _recorder;
    private readonly AimTracker _aim = new();
    private bool _wasHeld;
    private readonly LoopDiagnostics _diagnostics;
    private readonly LoopStatus _loopStatus = new();
    private volatile bool _running;
    private Thread? _thread;

    public FrameLoop(LoopDependencies dependencies)
    {
        ArgumentNullException.ThrowIfNull(dependencies);
        _recorder = dependencies.Recorder;
        _frames = dependencies.Frames ?? throw new ArgumentNullException("dependencies.Frames");
        _detector = dependencies.Detector ?? throw new ArgumentNullException("dependencies.Detector");
        _mouse = dependencies.Mouse ?? throw new ArgumentNullException("dependencies.Mouse");
        _isHeld = dependencies.IsHeld ?? throw new ArgumentNullException("dependencies.IsHeld");
        _currentSettings = dependencies.CurrentSettings ?? throw new ArgumentNullException("dependencies.CurrentSettings");
        _now = dependencies.Now ?? throw new ArgumentNullException("dependencies.Now");
        _onFault = dependencies.OnFault ?? throw new ArgumentNullException("dependencies.OnFault");
        _diagnostics = new LoopDiagnostics(_now, _loopStatus.SetLatency);
    }

    public LoopStatus LoopStatus => _loopStatus;

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
            LoopThreadBoost.Apply();
            Run();
        }
        catch (Exception error)
        {
            Log.Error($"the loop faulted: {error}");
            _onFault(error);
        }
    }

    private void Run()
    {
        using IdleTimer idleTimer = new();
        while (_running)
        {
            AppSettings settings = _currentSettings();
            bool held = _isHeld();
            _recorder?.ApplyEnabled(held && settings.Dataset.Enabled);
            _recorder?.AdvancePipeline();
            if (!held)
            {
                if (_wasHeld)
                {
                    Log.Debug("hotkey released");
                    _wasHeld = false;
                    // A frame held through the idle time makes the first frame after the next press old.
                    _frames.ReleaseHeldFrame();
                }

                _loopStatus.SetHeld(false);
                _aim.Reset();
                idleTimer.SleepMs(IdleSleepMs);
                continue;
            }

            if (!_wasHeld)
            {
                Log.Debug($"hotkey pressed; confidence={settings.Detect.Confidence}% input={_detector.InputWidth}x{_detector.InputHeight}");
                _diagnostics.Restart();
                _wasHeld = true;
            }

            _loopStatus.SetHeld(true);
            // The first value of a changed interpolation needs a serial round trip with a reply timeout of 100 ms.
            _mouse.SetInterpolation(settings.Mouse.Interpolation);
            CaptureOutcome status = _frames.TryAcquire(AcquireTimeoutMs, out CapturedFrame captured);
            if (status == CaptureOutcome.Timeout)
            {
                _diagnostics.RecordTimeout();
                continue;
            }

            if (status != CaptureOutcome.Ready)
            {
                _diagnostics.RecordNoFrame();
                continue;
            }

            DetectedFrame frame = _detector.Detect(captured, settings.Detect.Confidence / 100f);
            Size input = new(_detector.InputWidth, _detector.InputHeight);
            AimResult aim = _aim.Update(
                target: frame.Target,
                desktopTimestamp: frame.DesktopTimestamp,
                nowTimestamp: _now(),
                input: input,
                settings: settings.Aim);
            long aimedAt = _now();
            // Read the flag after the aim. A stop during the frame then suppresses its move.
            bool sent = aim.Outcome == AimOutcome.Move && _running;
            if (sent)
            {
                _mouse.Move(aim.Dx, aim.Dy);
            }

            long sentAt = sent ? _now() : aimedAt;
            FrameTimings timings = new(frame.DesktopTimestamp, frame.DetectStartTimestamp, frame.DetectedTimestamp, aimedAt, sentAt);
            _loopStatus.RecordFrame();
            _diagnostics.RecordFrame(frame, timings, input, aim, sent);

            // The recording comes last. It must not delay the move. The frame stays held until the next acquire.
            if (frame.Target is not null)
            {
                _recorder?.TryRecord(_frames);
            }
        }
    }
}
