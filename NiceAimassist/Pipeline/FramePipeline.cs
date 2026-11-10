using System.Diagnostics;
using NiceAimassist.Collect;
using NiceAimassist.Gpu;
using Silk.NET.Direct3D12;

namespace NiceAimassist.Pipeline;

public sealed unsafe class FramePipeline : IFramePipeline
{
    private const int WarmUpRuns = 3;

    private readonly GpuContext _gpu;
    private readonly TensorPrecision _outputPrecision;
    private readonly int _outputRows;
    private readonly Collector? _collector;

    private nint _env;
    private void* _dmlDevice;
    private nint _session;
    private nint _allocator;
    private nint _inputValue;
    private nint _outputValue;
    private nint _binding;
    private ID3D12Resource* _inputResource;
    private ID3D12Resource* _outputResource;
    private Readback? _readback;
    private Preprocessor? _preprocessor;
    private FrameSource? _frames;
    private bool _disposed;

    public FramePipeline(GpuContext gpu, string modelPath, Collector? collector = null)
    {
        ArgumentNullException.ThrowIfNull(gpu);
        ArgumentNullException.ThrowIfNull(modelPath);
        _collector = collector;
        if (!File.Exists(modelPath))
        {
            throw new FileNotFoundException("The ONNX model does not exist.", modelPath);
        }

        _gpu = gpu;
        try
        {
            _env = OrtNative.CreateEnv("NiceAimassist");
            _dmlDevice = DirectMl.CreateDevice(gpu.Device);
            _session = CreateSession(modelPath);

            TensorIo input = OrtNative.GetTensorIo(_session, input: true, 0);
            TensorIo output = OrtNative.GetTensorIo(_session, input: false, 0);
            if (OrtNative.GetIoCount(_session, input: true) != 1 || OrtNative.GetIoCount(_session, input: false) != 1)
            {
                throw new InvalidOperationException("Expected exactly one model input and one model output.");
            }

            ModelInput inputSpec = ModelContract.ParseInput(input.Dims, input.ElementType);
            ModelOutput outputSpec = ModelContract.ParseOutput(output.Dims, output.ElementType);
            ModelContract.ValidateMetadata(
                OrtNative.GetCustomMetadata(_session, "dynamic"),
                OrtNative.GetCustomMetadata(_session, "end2end"),
                OrtNative.GetCustomMetadata(_session, "nms"));

            InputWidth = inputSpec.Width;
            InputHeight = inputSpec.Height;
            _outputRows = outputSpec.Rows;
            _outputPrecision = outputSpec.Precision;

            int inputElementBytes = ModelContract.ElementBytes(inputSpec.Precision);
            int outputElementBytes = ModelContract.ElementBytes(outputSpec.Precision);
            long[] inputShape = [1, 3, InputHeight, InputWidth];
            long[] outputShape = [1, _outputRows, ModelContract.OutputColumns];

            _allocator = OrtNative.CreateDirectMlAllocator(_session);
            _inputValue = OrtNative.CreateTensor(_allocator, inputShape, input.ElementType);
            _inputResource = DirectMlBuffer(_inputValue, 3L * InputWidth * InputHeight * inputElementBytes, "input");
            _outputValue = OrtNative.CreateTensor(_allocator, outputShape, output.ElementType);
            _outputResource = DirectMlBuffer(
                _outputValue, (long)_outputRows * ModelContract.OutputColumns * outputElementBytes, "output");

            _frames = new FrameSource(gpu, InputWidth, InputHeight);
            _preprocessor = new Preprocessor(gpu, _frames, inputSpec.Precision == TensorPrecision.Fp16);
            _readback = new Readback(gpu, _outputResource, _outputRows * ModelContract.OutputColumns * outputElementBytes);

            _binding = OrtNative.CreateIoBinding(_session);
            OrtNative.BindInput(_binding, input.Name, _inputValue);
            OrtNative.BindOutput(_binding, output.Name, _outputValue);

            Log.Info(
                $"model {Path.GetFileName(modelPath)}: input {InputWidth}x{InputHeight} {inputSpec.Precision}, "
                + $"output {_outputRows} rows {_outputPrecision}");

            for (int run = 0; run < WarmUpRuns; run++)
            {
                RunAndRead();
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

    public bool TryRun(int timeoutMs, float confidence, out FrameResult result)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        result = default;
        if (!_frames!.TryAcquire(timeoutMs, out long presentQpc))
        {
            return false;
        }

        long acquiredQpc = Stopwatch.GetTimestamp();
        _preprocessor!.Submit(_inputResource);
        CollectHeldFrame();
        ReadOnlySpan<byte> output = RunAndRead();
        Detection? target = DetectionDecoder.SelectNearest(output, _outputPrecision, _outputRows, InputWidth, InputHeight, confidence);
        long inferredQpc = Stopwatch.GetTimestamp();
        float modelBestScore = Log.IsEnabled(LogLevel.Debug)
            ? DetectionDecoder.MaxScore(output, _outputPrecision, _outputRows)
            : 0f;
        result = new FrameResult(presentQpc, acquiredQpc, inferredQpc, target, modelBestScore);
        return true;
    }

    private void CollectHeldFrame()
    {
        if (_collector is { Due: true } && _frames!.DesktopTexture is var desktop && desktop != null)
        {
            _collector.Capture(desktop);
        }
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

    private ReadOnlySpan<byte> RunAndRead()
    {
        OrtNative.RunWithBinding(_session, _binding);
        return _readback!.Read();
    }

    private nint CreateSession(string modelPath)
    {
        nint options = OrtNative.CreateSessionOptions();
        try
        {
            OrtNative.SetSequentialExecution(options);
            OrtNative.DisableMemoryPattern(options);
            OrtNative.SetIntraOpThreads(options, 1);
            OrtNative.AddSessionConfig(options, "session.intra_op.allow_spinning", "0");
            OrtNative.AddSessionConfig(options, "session.inter_op.allow_spinning", "0");
            OrtNative.AddSessionConfig(options, "ep.dml.enable_cpu_sync_spinning", "1");
            OrtNative.AppendDirectMl1(options, (nint)_dmlDevice, (nint)_gpu.ComputeQueue);
            return OrtNative.CreateSession(_env, modelPath, options);
        }
        finally
        {
            OrtNative.ReleaseSessionOptions(options);
        }
    }

    // The caller owns the reference that ORT returns.
    private ID3D12Resource* DirectMlBuffer(nint value, long requiredBytes, string role)
    {
        ID3D12Resource* resource = (ID3D12Resource*)OrtNative.GetD3D12Resource(_allocator, OrtNative.GetTensorData(value));
        try
        {
            ResourceDesc description = resource->GetDesc();
            if (description.Dimension != ResourceDimension.Buffer)
            {
                throw new InvalidOperationException($"The DirectML {role} resource must be a buffer.");
            }

            if (description.Width < (ulong)requiredBytes)
            {
                throw new InvalidOperationException(
                    $"The DirectML {role} resource is {description.Width} bytes; it needs {requiredBytes}.");
            }

            if ((description.Flags & ResourceFlags.AllowUnorderedAccess) == 0)
            {
                throw new InvalidOperationException($"The DirectML {role} resource does not allow unordered access.");
            }
        }
        catch
        {
            Com.Release(ref resource);
            throw;
        }

        return resource;
    }

    // Order matters: the binding and tensors go before the allocator, the allocator before the session,
    // and the DirectML device after the session. The readback, preprocessor and frame source come last
    // because the queue may still touch them until it is idle.
    private void ReleaseAll()
    {
        _gpu.WaitIdle();

        if (_binding != 0)
        {
            OrtNative.ReleaseIoBinding(_binding);
            _binding = 0;
        }

        if (_inputValue != 0)
        {
            OrtNative.ReleaseValue(_inputValue);
            _inputValue = 0;
        }

        if (_outputValue != 0)
        {
            OrtNative.ReleaseValue(_outputValue);
            _outputValue = 0;
        }

        if (_allocator != 0)
        {
            OrtNative.ReleaseAllocator(_allocator);
            _allocator = 0;
        }

        if (_session != 0)
        {
            OrtNative.ReleaseSession(_session);
            _session = 0;
        }

        if (_dmlDevice != null)
        {
            ((Silk.NET.Core.Native.IUnknown*)_dmlDevice)->Release();
            _dmlDevice = null;
        }

        if (_env != 0)
        {
            OrtNative.ReleaseEnv(_env);
            _env = 0;
        }

        _readback?.Dispose();
        _readback = null;
        Com.Release(ref _outputResource);
        Com.Release(ref _inputResource);
        _preprocessor?.Dispose();
        _preprocessor = null;
        _frames?.Dispose();
        _frames = null;
    }
}
