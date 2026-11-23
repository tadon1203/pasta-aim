using Pstm.Capture;
using Pstm.Dataset;
using Pstm.Detect;
using Pstm.Mouse;
using Pstm.Settings;

namespace Pstm.Loop;

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
