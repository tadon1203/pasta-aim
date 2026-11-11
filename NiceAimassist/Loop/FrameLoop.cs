using System.Drawing;
using System.Runtime.InteropServices;
using NiceAimassist.Aim;
using NiceAimassist.Collect;
using NiceAimassist.Settings;
using NiceAimassist.Capture;
using NiceAimassist.Detect;
using NiceAimassist.Shared.Logging;
using NiceAimassist.Mouse;

namespace NiceAimassist.Loop;

/// <summary>Runs the aim loop on one dedicated thread.</summary>
public sealed partial class FrameLoop
{
    private const int IdleSleepMs = 1;

    private readonly IDetector _detector;
    private readonly IMouse _mouse;
    private readonly Func<bool> _isHeld;
    private readonly Func<AppSettings> _currentSettings;
    private readonly Func<long> _now;
    private readonly Action<Exception> _onFault;
    private readonly ICollectControl? _collector;
    private readonly AimController _aim = new();
    private bool _wasHeld;
    private readonly LoopDiagnostics _diagnostics;
    private readonly LoopStatus _loopStatus = new();
    private volatile bool _running;
    private Thread? _thread;
    private bool _interpolationSent;
    private Interpolation _sentInterpolation;

    /// <param name="now">Source of the Stopwatch timestamps. Usually <c>Stopwatch.GetTimestamp</c>.</param>
    /// <param name="onFault">Called on the loop thread when the detector or the mouse throws. The loop ends after the call.</param>
    public FrameLoop(
        IDetector detector,
        IMouse mouse,
        Func<bool> isHeld,
        Func<AppSettings> currentSettings,
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
        _diagnostics = new LoopDiagnostics(_now, _loopStatus);
    }

    public LoopStatus LoopStatus => _loopStatus;

    public void Start()
    {
        if (_thread is not null)
        {
            throw new InvalidOperationException("the loop is already started");
        }

        _running = true;
        _thread = new Thread(ThreadMain) { Name = "loop", IsBackground = true, Priority = ThreadPriority.Highest };
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
        uint taskIndex = 0;
        nint mmcss = AvSetMmThreadCharacteristicsW("Games", ref taskIndex);
        if (mmcss == 0)
        {
            Log.Warn($"AvSetMmThreadCharacteristicsW failed for task Games ({Marshal.GetLastPInvokeError()}); the loop keeps its thread priority only");
        }

        try
        {
            Loop();
        }
        catch (Exception error)
        {
            Log.Error($"the loop faulted: {error}");
            _onFault(error);
        }
        finally
        {
            // The revert must run on the thread that set the characteristics.
            if (mmcss != 0)
            {
                AvRevertMmThreadCharacteristics(mmcss);
            }
        }
    }

    private void Loop()
    {
        using IdleTimer idleTimer = new();
        while (_running)
        {
            AppSettings settings = _currentSettings();
            bool held = _isHeld();
            UpdateCollector(settings, held);
            if (!held)
            {
                if (_wasHeld)
                {
                    Log.Debug("hold key released");
                    _wasHeld = false;
                }

                _loopStatus.SetHeld(false);
                _aim.Reset();
                idleTimer.SleepMs(IdleSleepMs);
                continue;
            }

            if (!_wasHeld)
            {
                Log.Debug($"hold key pressed; confidence={settings.Detect.Confidence}% input={_detector.InputWidth}x{_detector.InputHeight}");
                _diagnostics.Restart();
                _wasHeld = true;
            }

            _loopStatus.SetHeld(true);
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
            _loopStatus.RecordFrame();
            _diagnostics.RecordFrame(frame, timings, aim, sent);
        }
    }

    [LibraryImport("avrt.dll", SetLastError = true, StringMarshalling = StringMarshalling.Utf16)]
    private static partial nint AvSetMmThreadCharacteristicsW(string taskName, ref uint taskIndex);

    [LibraryImport("avrt.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool AvRevertMmThreadCharacteristics(nint handle);

    // Collection runs only while the hold key is down. It uses the same frames as the aim.
    private void UpdateCollector(AppSettings settings, bool held)
    {
        if (_collector is null)
        {
            return;
        }

        _loopStatus.Collect = _collector.Update(settings.Collect, held);
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
