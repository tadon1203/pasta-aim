using System.Diagnostics;
using System.Drawing;
using NiceAimassist.Capture;
using NiceAimassist.Detect.Pipeline;
using NiceAimassist.Detect.Runtime;
using NiceAimassist.Shared.Gpu;
using NiceAimassist.Shared.Logging;

namespace NiceAimassist.Detect;

public sealed unsafe class Detector : IDetector
{
    private const int WarmUpRuns = 3;
    private const int AcquireTimeoutMs = 100;

    private readonly IFrameCollector? _collector;
    private OnnxSession? _session;
    private OutputReader? _outputReader;
    private Preprocessor? _preprocessor;
    private GpuTimer? _timer;
    private DesktopCapture? _capture;
    private bool _disposed;

    public Detector(GpuDevices gpu, string modelPath, IFrameCollector? collector = null)
    {
        ArgumentNullException.ThrowIfNull(gpu);
        ArgumentNullException.ThrowIfNull(modelPath);
        _collector = collector;
        try
        {
            _session = new OnnxSession(gpu, modelPath);
            InputWidth = _session.Input.Width;
            InputHeight = _session.Input.Height;

            Log.Info(
                $"model {Path.GetFileName(modelPath)}: input {InputWidth}x{InputHeight} {_session.Input.Precision}, "
                + $"output {_session.Output.Rows} rows {_session.Output.Precision}");

            _capture = new DesktopCapture(gpu, InputWidth, InputHeight);
            _timer = new GpuTimer(gpu);
            _preprocessor = new Preprocessor(gpu, _capture, _session.Input.Precision, _timer);
            _outputReader = new OutputReader(gpu, _session.OutputResource, checked((int)_session.Output.ByteLength), _timer);

            for (int run = 0; run < WarmUpRuns; run++)
            {
                InferAndRead();
            }
        }
        catch
        {
            ReleaseAll();
            throw;
        }
    }

    public int InputWidth { get; }

    public int InputHeight { get; }

    public CaptureStatus TryDetect(float confidence, out DetectedFrame result)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        result = default;
        CaptureStatus status = _capture!.TryAcquire(AcquireTimeoutMs, out CapturedFrame frame);
        if (status != CaptureStatus.Ready)
        {
            return status;
        }

        long capturedTimestamp = Stopwatch.GetTimestamp();
        _preprocessor!.Submit(frame, _session!.InputResource);
        long submitted = Stopwatch.GetTimestamp();
        _session.Run();
        long ran = Stopwatch.GetTimestamp();
        ReadOnlySpan<byte> output = _outputReader!.Read();
        long gpu = _timer!.ReadElapsedStopwatchTicks();
        long read = Stopwatch.GetTimestamp();
        DecodeResult decoded = DetectionDecoder.Decode(output, _session.Output, new Size(InputWidth, InputHeight), confidence);
        long detectedTimestamp = Stopwatch.GetTimestamp();
        if (decoded.Nearest is not null)
        {
            CollectHeldFrame();
        }

        DetectBreakdown breakdown = new(submitted - capturedTimestamp, ran - submitted, read - ran, detectedTimestamp - read, gpu);
        result = new DetectedFrame(
            frame.DesktopTimestamp, capturedTimestamp, detectedTimestamp, decoded.Nearest, decoded.TopScore, breakdown);
        return CaptureStatus.Ready;
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        ReleaseAll();
    }

    private void CollectHeldFrame()
    {
        if (_collector is null || !_collector.IsDue)
        {
            return;
        }

        var desktop = _capture!.DesktopTexture;
        if (desktop == null)
        {
            return;
        }

        _collector.Collect(desktop);
    }

    private ReadOnlySpan<byte> InferAndRead()
    {
        _session!.Run();
        return _outputReader!.Read();
    }

    // The session waits for the queue before it releases anything. The output reader, the preprocessor, the timer and
    // the capture can still be used by the queue until then. Therefore, they are released after the session.
    // The output reader and the preprocessor hold command lists that write to the timer. The timer is released after them.
    private void ReleaseAll()
    {
        _session?.Dispose();
        _session = null;
        _outputReader?.Dispose();
        _outputReader = null;
        _preprocessor?.Dispose();
        _preprocessor = null;
        _timer?.Dispose();
        _timer = null;
        _capture?.Dispose();
        _capture = null;
    }
}
