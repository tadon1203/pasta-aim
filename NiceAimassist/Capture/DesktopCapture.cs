using System.Runtime.InteropServices;
using NiceAimassist.Shared.Gpu;
using Silk.NET.Core.Native;
using Silk.NET.Direct3D11;
using Silk.NET.Direct3D12;
using Silk.NET.DXGI;
using D3D11Box = Silk.NET.Direct3D11.Box;
using UnmanagedType = System.Runtime.InteropServices.UnmanagedType;
using D3D12FenceFlags = Silk.NET.Direct3D12.FenceFlags;

namespace NiceAimassist.Capture;

// A frame that is not submitted to the Preprocessor is dropped. The next copy then waits only for the last submitted frame.
public sealed unsafe partial class DesktopCapture : IDisposable
{
    private const int DxgiAccessLost = unchecked((int)0x887A0026);
    private const int DxgiWaitTimeout = unchecked((int)0x887A0027);
    private const uint GenericAll = 0x10000000;
    private const uint SharedResourceReadWrite = 0x80000001;

    private readonly GpuDevices _gpu;
    private ID3D11Texture2D* _sharedTexture;
    private ID3D11Fence* _copyDoneFence11;
    private ID3D11Fence* _computeDoneFence11;
    private ID3D12Resource* _sharedResource;
    private ID3D12Fence* _copyDoneFence12;
    private ID3D12Fence* _computeDoneFence12;
    private IDXGIOutputDuplication* _duplication;
    private bool _frameHeld;
    private IDXGIResource* _desktopResource;
    private ID3D11Texture2D* _desktopTexture;
    private bool _layoutValid;
    private D3D11Box _sourceBox;
    private ulong _sequence;
    private ulong _submittedSequence;
    private bool _disposed;

    public DesktopCapture(GpuDevices gpu, int inputWidth, int inputHeight)
    {
        _gpu = gpu;
        InputWidth = inputWidth;
        InputHeight = inputHeight;

        ID3D11Device* captureDevice = gpu.CaptureDevice;
        ID3D12Device* device = gpu.Device;
        ID3D11Device5* captureDevice5 = ComInterop.Query<ID3D11Device5>(captureDevice);
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
            ComInterop.Check(captureDevice->CreateTexture2D(&textureDesc, null, &texture), "CreateTexture2D (shared frame)");
            _sharedTexture = texture;
            _sharedResource = OpenSharedTexture(_sharedTexture, device);

            ID3D11Fence* ready;
            ComInterop.Check(
                captureDevice5->CreateFence(0, FenceFlag.Shared, ComInterop.IidOf<ID3D11Fence>(), (void**)&ready),
                "ID3D11Device5.CreateFence (capture ready)");
            _copyDoneFence11 = ready;
            _copyDoneFence12 = OpenSharedFence12(_copyDoneFence11, device);

            // The compute device creates this fence. The capture copy then waits on the GPU, not on the CPU.
            ID3D12Fence* computeDone;
            ComInterop.Check(
                device->CreateFence(0, D3D12FenceFlags.Shared, ComInterop.IidOf<ID3D12Fence>(), (void**)&computeDone),
                "ID3D12Device.CreateFence (compute done)");
            _computeDoneFence12 = computeDone;
            void* computeDoneHandle;
            ComInterop.Check(
                device->CreateSharedHandle((ID3D12DeviceChild*)_computeDoneFence12, null, GenericAll, (char*)null, &computeDoneHandle),
                "CreateSharedHandle (compute done fence)");
            try
            {
                ID3D11Fence* computeDone11;
                ComInterop.Check(
                    captureDevice5->OpenSharedFence(computeDoneHandle, ComInterop.IidOf<ID3D11Fence>(), (void**)&computeDone11),
                    "OpenSharedFence (compute done fence)");
                _computeDoneFence11 = computeDone11;
            }
            finally
            {
                CloseHandle((nint)computeDoneHandle);
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
            ComInterop.Release(ref captureDevice5);
        }
    }

    public int InputWidth { get; }

    public int InputHeight { get; }

    internal ID3D12Resource* SharedResource => _sharedResource;

    internal ID3D12Fence* CopyDoneFence => _copyDoneFence12;

    internal ID3D12Fence* ComputeDoneFence => _computeDoneFence12;

    /// <summary>The desktop image of the held frame. It is invalid after the next <see cref="TryAcquire"/>.</summary>
    internal ID3D11Texture2D* DesktopTexture => _desktopTexture;

    internal ulong Sequence => _sequence;

    internal ulong SubmittedSequence => _submittedSequence;

    internal void MarkSubmitted(ulong sequence) => _submittedSequence = sequence;

    internal CaptureStatus TryAcquire(int timeoutMs, out CapturedFrame frame)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        frame = default;

        if (_frameHeld)
        {
            // The duplicated surface must not outlive its frame. Release the wrappers before ReleaseFrame.
            DisposeDesktopImage();
            _frameHeld = false;
            int release = _duplication->ReleaseFrame();
            if (release == DxgiAccessLost)
            {
                RecreateDuplication();
                return CaptureStatus.Lost;
            }

            ComInterop.Check(release, "ReleaseFrame");
        }

        OutduplFrameInfo frameInfo;
        IDXGIResource* resource;
        int acquire = _duplication->AcquireNextFrame((uint)timeoutMs, &frameInfo, &resource);
        if (acquire == DxgiWaitTimeout)
        {
            return CaptureStatus.Timeout;
        }

        if (acquire == DxgiAccessLost)
        {
            RecreateDuplication();
            return CaptureStatus.Lost;
        }

        ComInterop.Check(acquire, "AcquireNextFrame");
        _frameHeld = true;

        if (frameInfo.LastPresentTime == 0)
        {
            ComInterop.Release(ref resource);
            return CaptureStatus.Unchanged;
        }

        _desktopResource = resource;
        _desktopTexture = ComInterop.Query<ID3D11Texture2D>(resource);
        if (!_layoutValid)
        {
            ValidateLayout(_desktopTexture);
        }

        ID3D11DeviceContext4* context = _gpu.CaptureContext4;
        if (_submittedSequence != 0)
        {
            ComInterop.Check(context->Wait(_computeDoneFence11, _submittedSequence), "ID3D11DeviceContext4.Wait");
        }

        D3D11Box sourceBox = _sourceBox;
        ((ID3D11DeviceContext*)context)->CopySubresourceRegion(
            (ID3D11Resource*)_sharedTexture, 0, 0, 0, 0, (ID3D11Resource*)_desktopTexture, 0, &sourceBox);
        _sequence++;
        ComInterop.Check(context->Signal(_copyDoneFence11, _sequence), "ID3D11DeviceContext4.Signal");
        // Without a flush, the copy and the signal may not reach the GPU. The compute queue can already wait for them.
        ((ID3D11DeviceContext*)context)->Flush();

        frame = new CapturedFrame(_sequence, frameInfo.LastPresentTime);
        return CaptureStatus.Ready;
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

    [LibraryImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool CloseHandle(nint handle);

    private void ReleaseAll()
    {
        DisposeDesktopImage();
        ComInterop.Release(ref _duplication);
        ComInterop.Release(ref _computeDoneFence11);
        ComInterop.Release(ref _computeDoneFence12);
        ComInterop.Release(ref _copyDoneFence11);
        ComInterop.Release(ref _copyDoneFence12);
        ComInterop.Release(ref _sharedResource);
        ComInterop.Release(ref _sharedTexture);
    }

    private static ID3D12Resource* OpenSharedTexture(ID3D11Texture2D* texture, ID3D12Device* device)
    {
        IDXGIResource1* dxgi = ComInterop.Query<IDXGIResource1>(texture);
        try
        {
            void* handle;
            ComInterop.Check(dxgi->CreateSharedHandle(null, SharedResourceReadWrite, (char*)null, &handle), "CreateSharedHandle (texture)");
            try
            {
                ID3D12Resource* resource;
                ComInterop.Check(device->OpenSharedHandle(handle, ComInterop.IidOf<ID3D12Resource>(), (void**)&resource), "OpenSharedHandle (texture)");
                return resource;
            }
            finally
            {
                CloseHandle((nint)handle);
            }
        }
        finally
        {
            ComInterop.Release(ref dxgi);
        }
    }

    private static ID3D12Fence* OpenSharedFence12(ID3D11Fence* fence, ID3D12Device* device)
    {
        void* handle;
        ComInterop.Check(fence->CreateSharedHandle(null, GenericAll, (char*)null, &handle), "CreateSharedHandle (ready fence)");
        try
        {
            ID3D12Fence* opened;
            ComInterop.Check(device->OpenSharedHandle(handle, ComInterop.IidOf<ID3D12Fence>(), (void**)&opened), "OpenSharedHandle (ready fence)");
            return opened;
        }
        finally
        {
            CloseHandle((nint)handle);
        }
    }

    private IDXGIOutputDuplication* DuplicateOutput()
    {
        IDXGIOutputDuplication* duplication;
        ComInterop.Check(_gpu.Output->DuplicateOutput((IUnknown*)_gpu.CaptureDevice, &duplication), "DuplicateOutput");
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
        ComInterop.Release(ref _duplication);
        _duplication = DuplicateOutput();
        _frameHeld = false;
        _layoutValid = false;
    }

    private void DisposeDesktopImage()
    {
        ComInterop.Release(ref _desktopTexture);
        ComInterop.Release(ref _desktopResource);
    }
}
