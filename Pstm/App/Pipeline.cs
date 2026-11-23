using System.Diagnostics;
using Pstm.Capture;
using Pstm.Samples;
using Pstm.Detect;
using Pstm.Detect.Model;
using Pstm.Detect.Runtime;
using Pstm.Hotkey;
using Pstm.Loop;
using Pstm.Mouse.Serial;
using Pstm.Settings;
using Pstm.Shared;
using Pstm.Shared.Gpu;
using Pstm.Shared.Logging;

namespace Pstm.App;

/// <summary>
/// Owns the capture, the model, the detector, the mouse, and the loop. It releases them in the one order that the
/// GPU pipeline allows. See <c>docs/gpu-frame-protocol.md</c>.
/// </summary>
internal sealed class Pipeline : IDisposable
{
    private GpuDevices? _gpu;
    private OnnxSession? _session;
    private DesktopCapture? _frames;
    private SampleRecorder? _recorder;
    private Detector? _detector;
    private SerialDevice? _device;
    private FrameLoop? _loop;

    public Pipeline(
        SettingsStore file,
        AppSettings startup,
        HotkeySet hotkey,
        Action<Exception> onFault)
    {
        try
        {
            Directory.CreateDirectory(ModelFolder.Location);
            string modelPath = ModelFolder.Resolve(startup.Detect.Model);
            _gpu = new GpuDevices(startup.Display.Adapter, startup.Display.Output);
            _session = new OnnxSession(_gpu, modelPath);
            _frames = new DesktopCapture(_gpu, _session.Input.Width, _session.Input.Height);
            _recorder = new SampleRecorder(
                _gpu, AppFolders.Samples, Stopwatch.GetTimestamp, Stopwatch.Frequency);
            _detector = new Detector(_gpu, _session, _frames);
            Log.Info($"model {Path.GetFileName(modelPath)}");
            _device = SerialDevice.Connect();
            Log.Info($"device on {_device.PortName} at {_device.BaudRate} baud");
            _loop = new FrameLoop(new LoopDependencies(
                Frames: _frames,
                Detector: _detector,
                Mouse: _device,
                IsHeld: hotkey.IsHeld,
                CurrentSettings: () => file.Current,
                Now: Stopwatch.GetTimestamp,
                OnFault: onFault,
                Recorder: _recorder));
        }
        catch
        {
            Dispose();
            throw;
        }
    }

    public FrameLoop Loop => _loop ?? throw new InvalidOperationException("the pipeline is not ready");

    public ISampleRecorder Recorder => _recorder ?? throw new InvalidOperationException("the pipeline is not ready");
    public void Start() => Loop.Start();

    // The loop comes first. The detector waits for the capture reads. The GPU goes last.
    public void Dispose()
    {
        _loop?.Stop();
        _detector?.Dispose();
        _session?.Dispose();
        _frames?.Dispose();
        _recorder?.Dispose();
        _gpu?.Dispose();
        _device?.Dispose();
    }
}
