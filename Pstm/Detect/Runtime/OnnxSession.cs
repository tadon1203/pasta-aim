using System.Runtime.InteropServices;
using Pstm.Detect.Model;
using Pstm.Shared.Gpu;
using Pstm.Shared.Logging;
using Silk.NET.Core.Native;
using Silk.NET.Direct3D12;

namespace Pstm.Detect.Runtime;

// The tensors are bound once. Every run reads the input tensor and writes the output tensor.
internal sealed unsafe class OnnxSession : IDisposable
{
    private readonly GpuDevices _gpu;
    private nint _env;
    private void* _dmlDevice;
    private nint _session;
    private nint _allocator;
    private nint _inputValue;
    private nint _outputValue;
    private nint _binding;
    private nint _runOptions;
    private ID3D12Resource* _inputResource;
    private ID3D12Resource* _outputResource;
    private bool _disposed;

    public OnnxSession(GpuDevices gpu, string modelPath)
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
            _env = OrtApi.CreateEnv("Pstm");
            _dmlDevice = DirectMl.CreateDevice(gpu.Device);
            try
            {
                _session = CreateSession(modelPath, directMlOnly: true);
                Log.Info("every model node runs on DirectML; graph capture is requested");
            }
            catch (InvalidOperationException error)
            {
                Log.Warn($"the model did not load with DirectML only; graph capture is off: {error.Message}");
                _session = CreateSession(modelPath, directMlOnly: false);
            }

            TensorIo input = OrtApi.GetTensorIo(_session, input: true, 0);
            TensorIo output = OrtApi.GetTensorIo(_session, input: false, 0);
            if (OrtApi.GetIoCount(_session, input: true) != 1 || OrtApi.GetIoCount(_session, input: false) != 1)
            {
                throw new InvalidOperationException("Expected exactly one model input and one model output.");
            }

            Input = ModelContract.ParseInput(input.Dims, input.ElementType);
            Output = ModelContract.ParseOutput(output.Dims, output.ElementType);
            ModelContract.ValidateMetadata(new ModelMetadata(
                Dynamic: OrtApi.GetCustomMetadata(_session, "dynamic"),
                EndToEnd: OrtApi.GetCustomMetadata(_session, "end2end"),
                Nms: OrtApi.GetCustomMetadata(_session, "nms")));

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

            _runOptions = OrtApi.CreateRunOptions();
            // The DirectML provider submits its work to the compute queue at the end of each run and does not wait for it.
            // The output copy goes on the same queue after the run, so the copy reads the finished output.
            OrtApi.AddRunConfig(_runOptions, "disable_synchronize_execution_providers", "1");
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
    internal void Run() => OrtApi.RunWithBinding(_session, _runOptions, _binding);

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
        ReleaseOnce(ref _runOptions, OrtApi.ReleaseRunOptions);
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

        ComInterop.Release(ref _outputResource);
        ComInterop.Release(ref _inputResource);
    }

    // Only the session creation can fail on the model. A failure of the option calls is a fault of the app, so it throws.
    private nint CreateSession(string modelPath, bool directMlOnly)
    {
        nint options = OrtApi.CreateSessionOptions();
        try
        {
            OrtApi.SetSequentialExecution(options);
            OrtApi.DisableMemoryPattern(options);
            OrtApi.SetIntraOpThreads(options, 1);
            OrtApi.AddSessionConfig(options, "session.intra_op.allow_spinning", "0");
            OrtApi.AddSessionConfig(options, "session.inter_op.allow_spinning", "0");
            if (directMlOnly)
            {
                OrtApi.AddSessionConfig(options, "session.disable_cpu_ep_fallback", "1");
                OrtApi.AddSessionConfig(options, "ep.dml.enable_graph_capture", "1");
            }

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
            ComInterop.Release(ref resource);
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

internal static unsafe partial class DirectMl
{
    // The IID of IDMLDevice.
    private static readonly Guid DeviceIid = new("6dbd6437-96fd-423f-a98c-ae5e7c2a573f");

    [LibraryImport("DirectML.dll")]
    private static partial int DMLCreateDevice(ID3D12Device* d3d12Device, uint flags, Guid* riid, void** device);

    /// <summary>Returns an IDMLDevice. The caller releases it.</summary>
    internal static void* CreateDevice(ID3D12Device* device)
    {
        void* result;
        Guid iid = DeviceIid;
        ComInterop.Check(DMLCreateDevice(device, 0, &iid, &result), "DMLCreateDevice");
        return result;
    }
}
