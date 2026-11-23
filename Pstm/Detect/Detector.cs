using System.Diagnostics;
using System.Drawing;
using Pstm.Capture;
using Pstm.Detect.Pipeline;
using Pstm.Detect.Runtime;
using Pstm.Shared.Gpu;
using Pstm.Shared.Logging;

namespace Pstm.Detect;

// This object owns the preprocessor, the output reader, and the timer. The caller owns the session and the capture.
// Dispose this object before the session and the capture.
public sealed unsafe class Detector : IDetector
{
    private const int WarmUpRuns = 3;

    private readonly GpuDevices _gpu;
    private readonly OnnxSession _session;
    private readonly GpuTimer _timer;
    private readonly Preprocessor _preprocessor;
    private readonly OutputReader _outputReader;
    private bool _disposed;

    internal Detector(GpuDevices gpu, OnnxSession session, DesktopCapture frames)
    {
        ArgumentNullException.ThrowIfNull(gpu);
        ArgumentNullException.ThrowIfNull(session);
        ArgumentNullException.ThrowIfNull(frames);
        _gpu = gpu;
        _session = session;
        InputWidth = session.Input.Width;
        InputHeight = session.Input.Height;

        Log.Info(
            $"model input {InputWidth}x{InputHeight} {session.Input.Precision}, "
            + $"output {session.Output.Rows} rows {session.Output.Precision}");

        GpuTimer timer = new(gpu);
        Preprocessor? preprocessor = null;
        OutputReader? outputReader = null;
        try
        {
            preprocessor = new Preprocessor(gpu, frames, session.Input.Precision, session.InputResource, timer);
            outputReader = new OutputReader(gpu, session.OutputResource, checked((int)session.Output.ByteLength), timer);
            _timer = timer;
            _preprocessor = preprocessor;
            _outputReader = outputReader;

            for (int run = 0; run < WarmUpRuns; run++)
            {
                InferAndRead();
            }
        }
        catch
        {
            ReleaseAll(timer, preprocessor, outputReader);
            throw;
        }
    }

    public int InputWidth { get; }

    public int InputHeight { get; }

    public DetectedFrame Detect(CapturedFrame frame, float confidence)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        long capturedTimestamp = Stopwatch.GetTimestamp();
        _preprocessor.Submit(frame);
        long submitted = Stopwatch.GetTimestamp();
        _session.Run();
        long ran = Stopwatch.GetTimestamp();
        ReadOnlySpan<byte> output = _outputReader.Read();
        long gpu = _timer.ReadElapsedStopwatchTicks();
        long read = Stopwatch.GetTimestamp();
        DecodeResult decoded = DetectionDecoder.Decode(output, _session.Output, new Size(InputWidth, InputHeight), confidence);
        long detectedTimestamp = Stopwatch.GetTimestamp();

        DetectTimings breakdown = new(submitted - capturedTimestamp, ran - submitted, read - ran, detectedTimestamp - read, gpu);
        return new DetectedFrame(
            frame.DesktopTimestamp, capturedTimestamp, detectedTimestamp, decoded.Nearest, decoded.TopScore, breakdown);
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        ReleaseAll(_timer, _preprocessor, _outputReader);
    }

    private ReadOnlySpan<byte> InferAndRead()
    {
        _session.Run();
        return _outputReader.Read();
    }

    // The queue can still use the output reader, the preprocessor, and the timer. Therefore, wait for the queue first.
    // The output reader and the preprocessor hold command lists that write to the timer. The timer is released after them.
    private void ReleaseAll(GpuTimer timer, Preprocessor? preprocessor, OutputReader? outputReader)
    {
        _gpu.WaitIdle();
        outputReader?.Dispose();
        preprocessor?.Dispose();
        timer.Dispose();
    }
}
