using NiceAimassist.Display;
using Silk.NET.Core.Native;
using Silk.NET.Direct3D12;

namespace NiceAimassist.Detect;

// The tensors are bound once. Every run reads the input tensor and writes the output tensor.
internal sealed unsafe class OnnxSession : IDisposable
{
    private readonly DisplayDevices _gpu;
    private nint _env;
    private void* _dmlDevice;
    private nint _session;
    private nint _allocator;
    private nint _inputValue;
    private nint _outputValue;
    private nint _binding;
    private ID3D12Resource* _inputResource;
    private ID3D12Resource* _outputResource;
    private bool _disposed;

    public OnnxSession(DisplayDevices gpu, string modelPath)
    {
        ArgumentNullException.ThrowIfNull(gpu);
        ArgumentNullException.ThrowIfNull(modelPath);
        if (!File.Exists(modelPath))
        {
            throw new FileNotFoundException("The ONNX model does not exist.", modelPath);
        }

        _gpu = gpu;
        try
        {
            _env = OrtApi.CreateEnv("NiceAimassist");
            _dmlDevice = DirectMl.CreateDevice(gpu.Device);
            _session = CreateSession(modelPath);

            TensorIo input = OrtApi.GetTensorIo(_session, input: true, 0);
            TensorIo output = OrtApi.GetTensorIo(_session, input: false, 0);
            if (OrtApi.GetIoCount(_session, input: true) != 1 || OrtApi.GetIoCount(_session, input: false) != 1)
            {
                throw new InvalidOperationException("Expected exactly one model input and one model output.");
            }

            Input = ModelContract.ParseInput(input.Dims, input.ElementType);
            Output = ModelContract.ParseOutput(output.Dims, output.ElementType);
            ModelContract.ValidateMetadata(
                OrtApi.GetCustomMetadata(_session, "dynamic"),
                OrtApi.GetCustomMetadata(_session, "end2end"),
                OrtApi.GetCustomMetadata(_session, "nms"));

            long[] inputShape = [1, 3, Input.Height, Input.Width];
            long[] outputShape = [1, Output.Rows, ModelContract.OutputColumns];

            _allocator = OrtApi.CreateDirectMlAllocator(_session);
            _inputValue = OrtApi.CreateTensor(_allocator, inputShape, input.ElementType);
            _inputResource = DirectMlBuffer(_inputValue, Input.ByteLength, "input");
            _outputValue = OrtApi.CreateTensor(_allocator, outputShape, output.ElementType);
            _outputResource = DirectMlBuffer(_outputValue, Output.ByteLength, "output");

            _binding = OrtApi.CreateIoBinding(_session);
            OrtApi.BindInput(_binding, input.Name, _inputValue);
            OrtApi.BindOutput(_binding, output.Name, _outputValue);
        }
        catch
        {
            ReleaseAll();
            throw;
        }
    }

    internal ModelInput Input { get; }

    internal ModelOutput Output { get; }

    /// <summary>The D3D12 buffer of the input tensor. The session owns it.</summary>
    internal ID3D12Resource* InputResource => _inputResource;

    /// <summary>The D3D12 buffer of the output tensor. The session owns it.</summary>
    internal ID3D12Resource* OutputResource => _outputResource;

    /// <summary>Runs the model on the input tensor. The caller reads the output tensor after the run.</summary>
    internal void Run() => OrtApi.RunWithBinding(_session, _binding);

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        ReleaseAll();
    }

    // The order matters. Release the binding and the tensors before the allocator. Release the allocator before the session.
    // Release the DirectML device after the session. The queue can use the tensor buffers until it is idle, so wait first.
    private void ReleaseAll()
    {
        _gpu.WaitIdle();

        ReleaseOnce(ref _binding, OrtApi.ReleaseIoBinding);
        ReleaseOnce(ref _inputValue, OrtApi.ReleaseValue);
        ReleaseOnce(ref _outputValue, OrtApi.ReleaseValue);
        ReleaseOnce(ref _allocator, OrtApi.ReleaseAllocator);
        ReleaseOnce(ref _session, OrtApi.ReleaseSession);

        if (_dmlDevice != null)
        {
            ((IUnknown*)_dmlDevice)->Release();
            _dmlDevice = null;
        }

        ReleaseOnce(ref _env, OrtApi.ReleaseEnv);

        Com.Release(ref _outputResource);
        Com.Release(ref _inputResource);
    }

    private nint CreateSession(string modelPath)
    {
        nint options = OrtApi.CreateSessionOptions();
        try
        {
            OrtApi.SetSequentialExecution(options);
            OrtApi.DisableMemoryPattern(options);
            OrtApi.SetIntraOpThreads(options, 1);
            OrtApi.AddSessionConfig(options, "session.intra_op.allow_spinning", "0");
            OrtApi.AddSessionConfig(options, "session.inter_op.allow_spinning", "0");
            OrtApi.AddSessionConfig(options, "ep.dml.enable_cpu_sync_spinning", "1");
            OrtApi.AppendDirectMl1(options, (nint)_dmlDevice, (nint)_gpu.ComputeQueue);
            return OrtApi.CreateSession(_env, modelPath, options);
        }
        finally
        {
            OrtApi.ReleaseSessionOptions(options);
        }
    }

    // The caller owns the reference that ORT returns.
    private ID3D12Resource* DirectMlBuffer(nint value, long requiredBytes, string role)
    {
        ID3D12Resource* resource = (ID3D12Resource*)OrtApi.GetD3D12Resource(_allocator, OrtApi.GetTensorData(value));
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

    private static void ReleaseOnce(ref nint handle, Action<nint> release)
    {
        if (handle != 0)
        {
            release(handle);
            handle = 0;
        }
    }
}
