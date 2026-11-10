using Silk.NET.Core.Native;
using Silk.NET.Direct3D11;
using Silk.NET.Direct3D12;
using Silk.NET.DXGI;
using D3D11Box = Silk.NET.Direct3D11.Box;
using D3D12FenceFlags = Silk.NET.Direct3D12.FenceFlags;

namespace NiceAimassist.Gpu;

// A frame that is not submitted to the Preprocessor is dropped. The next copy then waits only for the last submitted frame.
public sealed unsafe class FrameSource : IDisposable
{
    private readonly GpuContext _gpu;
    private ID3D11Texture2D* _sharedTexture;
    private ID3D11Fence* _readyFence11;
    private ID3D11Fence* _reuseFence11;
    private ID3D12Resource* _sharedResource;
    private ID3D12Fence* _readyFence12;
    private ID3D12Fence* _reuseFence12;
    private IDXGIOutputDuplication* _duplication;
    private bool _frameHeld;
    private IDXGIResource* _desktopResource;
    private ID3D11Texture2D* _desktopTexture;
    private bool _layoutValid;
    private D3D11Box _sourceBox;
    private ulong _sequence;
    private ulong _submittedSequence;
    private bool _disposed;

    public FrameSource(GpuContext gpu, int inputWidth, int inputHeight)
    {
        _gpu = gpu;
        InputWidth = inputWidth;
        InputHeight = inputHeight;

        ID3D11Device* captureDevice = gpu.CaptureDevice;
        ID3D12Device* device = gpu.Device;
        ID3D11Device5* captureDevice5 = Com.Query<ID3D11Device5>(captureDevice);
        try
        {
            Texture2DDesc textureDesc = new()
            {
                Width = (uint)inputWidth,
                Height = (uint)inputHeight,
                MipLevels = 1,
                ArraySize = 1,
                Format = Format.FormatB8G8R8A8Unorm,
                SampleDesc = new SampleDesc(1, 0),
                Usage = Usage.Default,
                BindFlags = (uint)BindFlag.ShaderResource,
                CPUAccessFlags = 0,
                MiscFlags = (uint)(ResourceMiscFlag.Shared | ResourceMiscFlag.SharedNthandle),
            };
            ID3D11Texture2D* texture;
            Com.Check(captureDevice->CreateTexture2D(&textureDesc, null, &texture), "CreateTexture2D (shared frame)");
            _sharedTexture = texture;
            _sharedResource = OpenSharedTexture(_sharedTexture, device);

            ID3D11Fence* ready;
            Com.Check(
                captureDevice5->CreateFence(0, FenceFlag.Shared, Com.IidOf<ID3D11Fence>(), (void**)&ready),
                "ID3D11Device5.CreateFence (capture ready)");
            _readyFence11 = ready;
            _readyFence12 = OpenSharedFence12(_readyFence11, device);

            // Created on the compute device so that the capture copy waits on it on the GPU, not on the CPU.
            ID3D12Fence* reuse;
            Com.Check(
                device->CreateFence(0, D3D12FenceFlags.Shared, Com.IidOf<ID3D12Fence>(), (void**)&reuse),
                "ID3D12Device.CreateFence (reuse)");
            _reuseFence12 = reuse;
            void* reuseHandle;
            Com.Check(
                device->CreateSharedHandle((ID3D12DeviceChild*)_reuseFence12, null, Com.GenericAll, (char*)null, &reuseHandle),
                "CreateSharedHandle (reuse fence)");
            try
            {
                ID3D11Fence* reuse11;
                Com.Check(
                    captureDevice5->OpenSharedFence(reuseHandle, Com.IidOf<ID3D11Fence>(), (void**)&reuse11),
                    "OpenSharedFence (reuse fence)");
                _reuseFence11 = reuse11;
            }
            finally
            {
                NativeGpu.CloseSharedHandle(reuseHandle);
            }

            _duplication = DuplicateOutput();
        }
        catch
        {
            ReleaseAll();
            throw;
        }
        finally
        {
            Com.Release(ref captureDevice5);
        }
    }

    public int InputWidth { get; }

    public int InputHeight { get; }

    internal ID3D12Resource* SharedResource => _sharedResource;

    internal ID3D12Fence* CaptureReadyFence => _readyFence12;

    internal ID3D12Fence* ReuseFence => _reuseFence12;

    /// <summary>The desktop image of the held frame. It is invalid after the next <see cref="TryAcquire"/>.</summary>
    internal ID3D11Texture2D* DesktopTexture => _desktopTexture;

    internal ulong Sequence => _sequence;

    internal ulong SubmittedSequence => _submittedSequence;

    internal void MarkSubmitted(ulong sequence) => _submittedSequence = sequence;

    internal AcquireResult TryAcquire(int timeoutMs, out FrameTicket ticket)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        ticket = default;

        if (_frameHeld)
        {
            // The duplicated surface must not outlive the frame it came from, so the wrappers go before ReleaseFrame.
            DisposeDesktopImage();
            _frameHeld = false;
            int release = _duplication->ReleaseFrame();
            if (release == Com.DxgiAccessLost)
            {
                RecreateDuplication();
                return AcquireResult.DeviceLost;
            }

            Com.Check(release, "ReleaseFrame");
        }

        OutduplFrameInfo frameInfo;
        IDXGIResource* resource;
        int acquire = _duplication->AcquireNextFrame((uint)timeoutMs, &frameInfo, &resource);
        if (acquire == Com.DxgiWaitTimeout)
        {
            return AcquireResult.Timeout;
        }

        if (acquire == Com.DxgiAccessLost)
        {
            RecreateDuplication();
            return AcquireResult.DeviceLost;
        }

        Com.Check(acquire, "AcquireNextFrame");
        _frameHeld = true;

        if (frameInfo.LastPresentTime == 0)
        {
            Com.Release(ref resource);
            return AcquireResult.NoNewImage;
        }

        _desktopResource = resource;
        _desktopTexture = Com.Query<ID3D11Texture2D>(resource);
        if (!_layoutValid)
        {
            ValidateLayout(_desktopTexture);
        }

        ID3D11DeviceContext4* context = _gpu.CaptureContext4;
        if (_submittedSequence != 0)
        {
            Com.Check(context->Wait(_reuseFence11, _submittedSequence), "ID3D11DeviceContext4.Wait");
        }

        D3D11Box sourceBox = _sourceBox;
        ((ID3D11DeviceContext*)context)->CopySubresourceRegion(
            (ID3D11Resource*)_sharedTexture, 0, 0, 0, 0, (ID3D11Resource*)_desktopTexture, 0, &sourceBox);
        _sequence++;
        Com.Check(context->Signal(_readyFence11, _sequence), "ID3D11DeviceContext4.Signal");
        // Without a flush, the copy and the signal may not reach the GPU while the compute queue already waits on them.
        ((ID3D11DeviceContext*)context)->Flush();

        ticket = new FrameTicket(_sequence, frameInfo.LastPresentTime);
        return AcquireResult.Frame;
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        DisposeDesktopImage();
        if (_frameHeld)
        {
            _ = _duplication->ReleaseFrame();
            _frameHeld = false;
        }

        ReleaseAll();
    }

    private void ReleaseAll()
    {
        DisposeDesktopImage();
        Com.Release(ref _duplication);
        Com.Release(ref _reuseFence11);
        Com.Release(ref _reuseFence12);
        Com.Release(ref _readyFence11);
        Com.Release(ref _readyFence12);
        Com.Release(ref _sharedResource);
        Com.Release(ref _sharedTexture);
    }

    private static ID3D12Resource* OpenSharedTexture(ID3D11Texture2D* texture, ID3D12Device* device)
    {
        IDXGIResource1* dxgi = Com.Query<IDXGIResource1>(texture);
        try
        {
            void* handle;
            Com.Check(dxgi->CreateSharedHandle(null, Com.SharedResourceReadWrite, (char*)null, &handle), "CreateSharedHandle (texture)");
            try
            {
                ID3D12Resource* resource;
                Com.Check(device->OpenSharedHandle(handle, Com.IidOf<ID3D12Resource>(), (void**)&resource), "OpenSharedHandle (texture)");
                return resource;
            }
            finally
            {
                NativeGpu.CloseSharedHandle(handle);
            }
        }
        finally
        {
            Com.Release(ref dxgi);
        }
    }

    private static ID3D12Fence* OpenSharedFence12(ID3D11Fence* fence, ID3D12Device* device)
    {
        void* handle;
        Com.Check(fence->CreateSharedHandle(null, Com.GenericAll, (char*)null, &handle), "CreateSharedHandle (ready fence)");
        try
        {
            ID3D12Fence* opened;
            Com.Check(device->OpenSharedHandle(handle, Com.IidOf<ID3D12Fence>(), (void**)&opened), "OpenSharedHandle (ready fence)");
            return opened;
        }
        finally
        {
            NativeGpu.CloseSharedHandle(handle);
        }
    }

    private IDXGIOutputDuplication* DuplicateOutput()
    {
        IDXGIOutputDuplication* duplication;
        Com.Check(_gpu.Output->DuplicateOutput((IUnknown*)_gpu.CaptureDevice, &duplication), "DuplicateOutput");
        return duplication;
    }

    private void ValidateLayout(ID3D11Texture2D* texture)
    {
        Texture2DDesc desc;
        texture->GetDesc(&desc);
        if (desc.Format != Format.FormatB8G8R8A8Unorm)
        {
            throw new InvalidOperationException(
                $"The desktop image format is {desc.Format}; only B8G8R8A8_UNorm can be captured. Turn off HDR and 10-bit color for this display.");
        }

        _sourceBox = CenterCrop.Box((int)desc.Width, (int)desc.Height, InputWidth, InputHeight);
        _layoutValid = true;
    }

    private void RecreateDuplication()
    {
        DisposeDesktopImage();
        Com.Release(ref _duplication);
        _duplication = DuplicateOutput();
        _frameHeld = false;
        _layoutValid = false;
    }

    private void DisposeDesktopImage()
    {
        Com.Release(ref _desktopTexture);
        Com.Release(ref _desktopResource);
    }
}
