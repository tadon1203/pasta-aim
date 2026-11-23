using System.Diagnostics;
using NiceAimassist.Capture;
using NiceAimassist.Dataset;
using NiceAimassist.Detect;
using NiceAimassist.Detect.Model;
using NiceAimassist.Detect.Runtime;
using NiceAimassist.Hotkey;
using NiceAimassist.Loop;
using NiceAimassist.Mouse.Makcu;
using NiceAimassist.Settings;
using NiceAimassist.Shared.Gpu;
using NiceAimassist.Shared.Logging;

namespace NiceAimassist.App;

/// <summary>
/// Owns the capture, the model, the detector, the mouse, and the loop. It releases them in the one order that the
/// GPU pipeline allows. See <c>docs/gpu-frame-protocol.md</c>.
/// </summary>
internal sealed class Pipeline : IDisposable
{
    private GpuDevices? _gpu;
    private OnnxSession? _session;
    private DesktopCapture? _frames;
    private DatasetRecorder? _recorder;
    private Detector? _detector;
    private MakcuDevice? _device;
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
            _recorder = new DatasetRecorder(
                _gpu, Path.Combine(AppContext.BaseDirectory, "dataset"), Stopwatch.GetTimestamp, Stopwatch.Frequency);
            _detector = new Detector(_gpu, _session, _frames);
            Log.Info($"model {Path.GetFileName(modelPath)}");
            _device = MakcuDevice.Connect();
            Log.Info($"MAKCU on {_device.PortName} at {_device.BaudRate} baud");
            _loop = new FrameLoop(
                _frames,
                _detector,
                _device,
                hotkey.IsHeld,
                () => file.Current,
                Stopwatch.GetTimestamp,
                onFault,
                _recorder);
        }
        catch
        {
            Dispose();
            throw;
        }
    }

    public FrameLoop Loop => _loop ?? throw new InvalidOperationException("the pipeline is not ready");

    public IDatasetRecorder Recorder => _recorder ?? throw new InvalidOperationException("the pipeline is not ready");
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
