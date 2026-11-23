using NiceAimassist.Capture;
using NiceAimassist.Dataset;
using NiceAimassist.Detect;
using NiceAimassist.Mouse;
using NiceAimassist.Settings;

namespace NiceAimassist.Loop;

/// <param name="OnFault">Called on the loop thread when the frame source, the detector, or the mouse throws. The loop ends after the call.</param>
public sealed record LoopDependencies(
    IFrameSource Frames,
    IDetector Detector,
    IMouse Mouse,
    Func<bool> IsHeld,
    Func<AppSettings> CurrentSettings,
    Func<long> Now,
    Action<Exception> OnFault,
    IDatasetRecorder? Recorder = null);
