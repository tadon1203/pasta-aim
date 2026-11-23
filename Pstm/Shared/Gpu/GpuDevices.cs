using Silk.NET.Core.Native;
using Silk.NET.Direct3D11;
using Silk.NET.Direct3D12;
using Silk.NET.DXGI;

namespace Pstm.Shared.Gpu;

// Dispose this after the DesktopCapture and the Preprocessor. Dispose waits for the compute queue to drain.
public sealed unsafe class GpuDevices : IDisposable
{
    private const uint D3D11SdkVersion = 7;
    private const int DxgiNotFound = unchecked((int)0x887A0002);

    private static readonly DXGI Dxgi = DXGI.GetApi(window: null);
    private static readonly D3D11 D3D11Api = D3D11.GetApi(window: null);
    private static readonly D3D12 D3D12Api = D3D12.GetApi();

    private IDXGIAdapter1* _adapter;
    private IDXGIOutput1* _output;
    private ID3D11Device* _captureDevice;
    private ID3D11DeviceContext4* _captureContext;
    private ID3D12Device* _device;
    private ID3D12CommandQueue* _computeQueue;
    private ID3D12Fence* _idleFence;
    private ulong _idleValue;
    private bool _disposed;

    public GpuDevices(long adapterLuid, int outputIndex)
    {
        try
        {
            IDXGIFactory1* factory;
            ComInterop.Check(Dxgi.CreateDXGIFactory1(ComInterop.IidOf<IDXGIFactory1>(), (void**)&factory), "CreateDXGIFactory1");
            try
            {
                _adapter = SelectAdapter(factory, adapterLuid);
            }
            finally
            {
                ComInterop.Release(ref factory);
            }

            _output = SelectOutput(_adapter, outputIndex);

            D3DFeatureLevel level = D3DFeatureLevel.Level111;
            ID3D11Device* captureDevice;
            ID3D11DeviceContext* immediate;
            ComInterop.Check(
                D3D11Api.CreateDevice(
                    (IDXGIAdapter*)_adapter, D3DDriverType.Unknown, 0, 0, &level, 1, D3D11SdkVersion, &captureDevice, null, &immediate),
                "D3D11CreateDevice");
            _captureDevice = captureDevice;
            try
            {
                _captureContext = ComInterop.Query<ID3D11DeviceContext4>(immediate);
            }
            finally
            {
                ComInterop.Release(ref immediate);
            }

            ID3D12Device* device;
            ComInterop.Check(
                D3D12Api.CreateDevice((IUnknown*)_adapter, D3DFeatureLevel.Level110, ComInterop.IidOf<ID3D12Device>(), (void**)&device),
                "D3D12CreateDevice");
            _device = device;

            CommandQueueDesc queueDesc = new()
            {
                Type = CommandListType.Compute,
                Priority = (int)CommandQueuePriority.High,
                Flags = CommandQueueFlags.None,
            };
            ID3D12CommandQueue* queue;
            ComInterop.Check(_device->CreateCommandQueue(&queueDesc, ComInterop.IidOf<ID3D12CommandQueue>(), (void**)&queue), "CreateCommandQueue");
            _computeQueue = queue;

            ID3D12Fence* fence;
            ComInterop.Check(_device->CreateFence(0, FenceFlags.None, ComInterop.IidOf<ID3D12Fence>(), (void**)&fence), "CreateFence");
            _idleFence = fence;
        }
        catch
        {
            ReleaseAll();
            throw;
        }
    }

    internal ID3D12Device* Device
    {
        get
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            return _device;
        }
    }

    internal ID3D12CommandQueue* ComputeQueue
    {
        get
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            return _computeQueue;
        }
    }

    internal ID3D11Device* CaptureDevice
    {
        get
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            return _captureDevice;
        }
    }

    internal ID3D11DeviceContext4* CaptureContext
    {
        get
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            return _captureContext;
        }
    }

    internal IDXGIOutput1* Output
    {
        get
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            return _output;
        }
    }

    public void WaitIdle()
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        DrainQueue();
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        try
        {
            DrainQueue();
        }
        finally
        {
            ReleaseAll();
        }
    }

    private void DrainQueue()
    {
        ulong value = ++_idleValue;
        ComInterop.Check(_computeQueue->Signal(_idleFence, value), "Signal the idle fence");
        FenceSpin.Wait(_idleFence, value);
    }

    private void ReleaseAll()
    {
        ComInterop.Release(ref _idleFence);
        ComInterop.Release(ref _computeQueue);
        ComInterop.Release(ref _device);
        ComInterop.Release(ref _captureContext);
        ComInterop.Release(ref _captureDevice);
        ComInterop.Release(ref _output);
        ComInterop.Release(ref _adapter);
    }

    private static IDXGIAdapter1* SelectAdapter(IDXGIFactory1* factory, long adapterLuid)
    {
        for (uint index = 0; ; index++)
        {
            IDXGIAdapter1* adapter;
            int result = factory->EnumAdapters1(index, &adapter);
            if (result == DxgiNotFound)
            {
                break;
            }

            ComInterop.Check(result, "EnumAdapters1");
            if (adapterLuid == 0 || LuidOf(adapter) == adapterLuid)
            {
                return adapter;
            }

            ComInterop.Release(ref adapter);
        }

        throw new InvalidOperationException($"No DXGI adapter matches LUID {adapterLuid:X16}.");
    }

    private static long LuidOf(IDXGIAdapter1* adapter)
    {
        AdapterDesc1 description;
        ComInterop.Check(adapter->GetDesc1(&description), "GetDesc1");
        return ((long)description.AdapterLuid.High << 32) | description.AdapterLuid.Low;
    }

    private static IDXGIOutput1* SelectOutput(IDXGIAdapter1* adapter, int outputIndex)
    {
        IDXGIOutput* output;
        int result = adapter->EnumOutputs((uint)outputIndex, &output);
        if (result == DxgiNotFound)
        {
            throw new InvalidOperationException($"No display output {outputIndex} on the selected adapter.");
        }

        ComInterop.Check(result, "EnumOutputs");
        try
        {
            return ComInterop.Query<IDXGIOutput1>(output);
        }
        finally
        {
            ComInterop.Release(ref output);
        }
    }
}
