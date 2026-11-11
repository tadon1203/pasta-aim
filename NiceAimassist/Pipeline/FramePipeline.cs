using System.Diagnostics;
using System.Drawing;
using NiceAimassist.Gpu;
using Silk.NET.Direct3D12;

namespace NiceAimassist.Pipeline;

public sealed unsafe class FramePipeline : IFramePipeline
{
    private const int WarmUpRuns = 3;
    private const int AcquireTimeoutMs = 100;

    private readonly GpuContext _gpu;
    private readonly ModelOutput _output;
    private readonly IFrameObserver? _observer;

    private nint _env;
    private void* _dmlDevice;
    private nint _session;
    private nint _allocator;
    private nint _inputValue;
    private nint _outputValue;
    private nint _binding;
    private ID3D12Resource* _inputResource;
    private ID3D12Resource* _outputResource;
    private OutputReader? _outputReader;
    private Preprocessor? _preprocessor;
    private FrameSource? _frames;
    private bool _disposed;

    public FramePipeline(GpuContext gpu, string modelPath, IFrameObserver? observer = null)
    {
        ArgumentNullException.ThrowIfNull(gpu);
        ArgumentNullException.ThrowIfNull(modelPath);
        _observer = observer;
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
            _output = outputSpec;

            long[] inputShape = [1, 3, InputHeight, InputWidth];
            long[] outputShape = [1, _output.Rows, ModelContract.OutputColumns];

            _allocator = OrtNative.CreateDirectMlAllocator(_session);
            _inputValue = OrtNative.CreateTensor(_allocator, inputShape, input.ElementType);
            _inputResource = DirectMlBuffer(_inputValue, inputSpec.ByteLength, "input");
            _outputValue = OrtNative.CreateTensor(_allocator, outputShape, output.ElementType);
            _outputResource = DirectMlBuffer(_outputValue, _output.ByteLength, "output");

            _frames = new FrameSource(gpu, InputWidth, InputHeight);
            _preprocessor = new Preprocessor(gpu, _frames, inputSpec.Precision);
            _outputReader = new OutputReader(gpu, _outputResource, checked((int)_output.ByteLength));

            _binding = OrtNative.CreateIoBinding(_session);
            OrtNative.BindInput(_binding, input.Name, _inputValue);
            OrtNative.BindOutput(_binding, output.Name, _outputValue);

            Log.Info(
                $"model {Path.GetFileName(modelPath)}: input {InputWidth}x{InputHeight} {inputSpec.Precision}, "
                + $"output {_output.Rows} rows {_output.Precision}");

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

    public AcquireResult TryRun(float confidence, out FrameResult result)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        result = default;
        AcquireResult status = _frames!.TryAcquire(AcquireTimeoutMs, out CapturedFrame frame);
        if (status != AcquireResult.Frame)
        {
            return status;
        }

        long acquiredTimestamp = Stopwatch.GetTimestamp();
        _preprocessor!.Submit(frame, _inputResource);
        CollectHeldFrame();
        ReadOnlySpan<byte> output = InferAndRead();
        DecodeResult decoded = DetectionDecoder.Decode(output, _output, new Size(InputWidth, InputHeight), confidence);
        long inferredTimestamp = Stopwatch.GetTimestamp();
        result = new FrameResult(frame.DesktopTimestamp, acquiredTimestamp, inferredTimestamp, decoded.Nearest, decoded.MaxScore);
        return AcquireResult.Frame;
    }

    private void CollectHeldFrame()
    {
        if (_observer is null || !_observer.IsDue)
        {
            return;
        }

        var desktop = _frames!.DesktopTexture;
        if (desktop == null)
        {
            return;
        }

        _observer.Observe(desktop);
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

    private ReadOnlySpan<byte> InferAndRead()
    {
        OrtNative.RunWithBinding(_session, _binding);
        return _outputReader!.Read();
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

    // The order matters. Release the binding and the tensors before the allocator. Release the allocator before the session.
    // Release the DirectML device after the session. Release the output reader, preprocessor and frame source last.
    // The queue can still touch them until it is idle.
    private void ReleaseAll()
    {
        _gpu.WaitIdle();

        ReleaseOnce(ref _binding, OrtNative.ReleaseIoBinding);
        ReleaseOnce(ref _inputValue, OrtNative.ReleaseValue);
        ReleaseOnce(ref _outputValue, OrtNative.ReleaseValue);
        ReleaseOnce(ref _allocator, OrtNative.ReleaseAllocator);
        ReleaseOnce(ref _session, OrtNative.ReleaseSession);

        if (_dmlDevice != null)
        {
            ((Silk.NET.Core.Native.IUnknown*)_dmlDevice)->Release();
            _dmlDevice = null;
        }

        ReleaseOnce(ref _env, OrtNative.ReleaseEnv);

        _outputReader?.Dispose();
        _outputReader = null;
        Com.Release(ref _outputResource);
        Com.Release(ref _inputResource);
        _preprocessor?.Dispose();
        _preprocessor = null;
        _frames?.Dispose();
        _frames = null;
    }

    private static void ReleaseOnce(ref nint handle, Action<nint> release)
    {
        if (handle != 0)
        {
            release(handle);
            handle = 0;
        }
    }
}
