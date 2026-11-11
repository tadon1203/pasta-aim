using System.Diagnostics;
using System.Drawing;
using NiceAimassist.Capture;
using NiceAimassist.Display;

namespace NiceAimassist.Detect;

public sealed unsafe class Detector : IDetector
{
    private const int WarmUpRuns = 3;
    private const int AcquireTimeoutMs = 100;

    private readonly IFrameObserver? _observer;
    private OnnxSession? _session;
    private OutputReader? _outputReader;
    private Preprocessor? _preprocessor;
    private DesktopCapture? _capture;
    private bool _disposed;

    public Detector(DisplayDevices gpu, string modelPath, IFrameObserver? observer = null)
    {
        ArgumentNullException.ThrowIfNull(gpu);
        ArgumentNullException.ThrowIfNull(modelPath);
        _observer = observer;
        try
        {
            _session = new OnnxSession(gpu, modelPath);
            InputWidth = _session.Input.Width;
            InputHeight = _session.Input.Height;

            Log.Info(
                $"model {Path.GetFileName(modelPath)}: input {InputWidth}x{InputHeight} {_session.Input.Precision}, "
                + $"output {_session.Output.Rows} rows {_session.Output.Precision}");

            _capture = new DesktopCapture(gpu, InputWidth, InputHeight);
            _preprocessor = new Preprocessor(gpu, _capture, _session.Input.Precision);
            _outputReader = new OutputReader(gpu, _session.OutputResource, checked((int)_session.Output.ByteLength));

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
        CollectHeldFrame();
        ReadOnlySpan<byte> output = InferAndRead();
        DecodeResult decoded = DetectionDecoder.Decode(output, _session.Output, new Size(InputWidth, InputHeight), confidence);
        long detectedTimestamp = Stopwatch.GetTimestamp();
        result = new DetectedFrame(
            frame.DesktopTimestamp, capturedTimestamp, detectedTimestamp, decoded.Nearest, decoded.TopScore);
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
        if (_observer is null || !_observer.IsDue)
        {
            return;
        }

        var desktop = _capture!.DesktopTexture;
        if (desktop == null)
        {
            return;
        }

        _observer.Observe(desktop);
    }

    private ReadOnlySpan<byte> InferAndRead()
    {
        _session!.Run();
        return _outputReader!.Read();
    }

    // The session waits for the queue before it releases anything. The output reader, the preprocessor and the
    // capture can still be used by the queue until then. Therefore, they are released after the session.
    private void ReleaseAll()
    {
        _session?.Dispose();
        _session = null;
        _outputReader?.Dispose();
        _outputReader = null;
        _preprocessor?.Dispose();
        _preprocessor = null;
        _capture?.Dispose();
        _capture = null;
    }
}
