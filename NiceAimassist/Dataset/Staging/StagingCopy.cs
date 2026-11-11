using NiceAimassist.Capture;
using NiceAimassist.Shared.Gpu;
using Silk.NET.Direct3D11;
using Silk.NET.DXGI;
using D3D11Box = Silk.NET.Direct3D11.Box;

namespace NiceAimassist.Dataset.Staging;

// Every method except MarkCopied runs on the loop thread. That thread owns the immediate context.
internal sealed unsafe class StagingCopy : IDisposable
{
    private const uint DoNotWait = 0x100000;
    private const int DxgiWasStillDrawing = unchecked((int)0x887A000A);

    private readonly GpuDevices _gpu;
    private readonly StagingGate _gate = new();
    private ID3D11Texture2D* _stagingCrop;
    private ID3D11Texture2D* _stagingFull;

    public StagingCopy(GpuDevices gpu) => _gpu = gpu;

    public bool CanCopy => _gate.CanCopy;

    public bool IsPending => _gate.IsPending;

    /// <summary>Copies the centered square of <paramref name="size"/> px and the full desktop into the staging textures.</summary>
    public void Copy(ID3D11Texture2D* desktop, int size)
    {
        _gate.BeginCopy();
        try
        {
            Texture2DDesc desc;
            desktop->GetDesc(&desc);
            (_, _, int side) = CenterCrop.Square((int)desc.Width, (int)desc.Height, size);
            EnsureStaging(desc, side);

            ID3D11DeviceContext* context = _gpu.CaptureContext;
            D3D11Box box = CenterCrop.Box((int)desc.Width, (int)desc.Height, side, side);
            context->CopySubresourceRegion((ID3D11Resource*)_stagingCrop, 0, 0, 0, 0, (ID3D11Resource*)desktop, 0, &box);
            // The full copy goes last. When it is readable, the crop copy is readable too.
            context->CopyResource((ID3D11Resource*)_stagingFull, (ID3D11Resource*)desktop);
            // Without a flush, a DoNotWait map can report "still drawing" until the driver flushes.
            context->Flush();
        }
        catch
        {
            _gate.CancelCopy();
            throw;
        }
    }

    /// <summary>Forgets a GPU copy that is not mapped yet. Does nothing otherwise.</summary>
    public void DropPending()
    {
        if (_gate.IsPending)
        {
            _gate.CancelCopy();
        }
    }

    /// <summary>
    /// Maps the full copy without waiting, then maps the crop copy. Returns false while the GPU still draws the full copy.
    /// The textures stay mapped until <see cref="ReleaseIfCopied"/> or <see cref="CancelMap"/>.
    /// </summary>
    public bool TryMap(out MappedPixels full, out MappedPixels crop)
    {
        full = default;
        crop = default;
        if (!_gate.IsPending)
        {
            throw new InvalidOperationException("Map the staging textures: no copy is pending");
        }

        ID3D11DeviceContext* context = _gpu.CaptureContext;
        MappedSubresource fullMapped;
        int result = context->Map((ID3D11Resource*)_stagingFull, 0, Map.Read, DoNotWait, &fullMapped);
        if (result == DxgiWasStillDrawing)
        {
            return false;
        }

        ComInterop.Check(result, "Map the full staging texture");
        MappedSubresource cropMapped;
        try
        {
            ComInterop.Check(context->Map((ID3D11Resource*)_stagingCrop, 0, Map.Read, 0, &cropMapped), "Map the crop staging texture");
        }
        catch
        {
            context->Unmap((ID3D11Resource*)_stagingFull, 0);
            throw;
        }

        _gate.Map();
        full = Describe(_stagingFull, fullMapped);
        crop = Describe(_stagingCrop, cropMapped);
        return true;
    }

    /// <summary>Called on the writer thread after it copied the mapped rows.</summary>
    public void MarkCopied() => _gate.MarkCopied();

    /// <summary>Unmaps the textures once the writer copied them. Does nothing before that.</summary>
    public void ReleaseIfCopied()
    {
        if (!_gate.IsCopied)
        {
            return;
        }

        UnmapBoth();
        _gate.Release();
    }

    /// <summary>Unmaps the textures when the writer never receives the mapped sample.</summary>
    public void CancelMap()
    {
        UnmapBoth();
        _gate.CancelMap();
    }

    // Stopping the writer comes first. The writer must not read memory that this call unmaps.
    public void Dispose()
    {
        if (_gate.IsMapped)
        {
            UnmapBoth();
        }

        DisposeStaging();
    }

    private static MappedPixels Describe(ID3D11Texture2D* staging, MappedSubresource mapped)
    {
        Texture2DDesc desc;
        staging->GetDesc(&desc);
        return new MappedPixels((nint)mapped.PData, (int)mapped.RowPitch, (int)desc.Width, (int)desc.Height);
    }

    private void UnmapBoth()
    {
        ID3D11DeviceContext* context = _gpu.CaptureContext;
        context->Unmap((ID3D11Resource*)_stagingCrop, 0);
        context->Unmap((ID3D11Resource*)_stagingFull, 0);
    }

    // A failed rebuild keeps the old textures. They still match the old sizes.
    private void EnsureStaging(Texture2DDesc desktop, int side)
    {
        if (_stagingFull != null && _stagingCrop != null)
        {
            Texture2DDesc full;
            Texture2DDesc crop;
            _stagingFull->GetDesc(&full);
            _stagingCrop->GetDesc(&crop);
            if (full.Width == desktop.Width && full.Height == desktop.Height && crop.Width == (uint)side)
            {
                return;
            }
        }

        ID3D11Texture2D* newFull = CreateStaging(desktop.Width, desktop.Height, desktop.Format);
        ID3D11Texture2D* newCrop;
        try
        {
            newCrop = CreateStaging((uint)side, (uint)side, desktop.Format);
        }
        catch
        {
            ComInterop.Release(ref newFull);
            throw;
        }

        DisposeStaging();
        _stagingFull = newFull;
        _stagingCrop = newCrop;
    }

    private ID3D11Texture2D* CreateStaging(uint width, uint height, Format format)
    {
        Texture2DDesc desc = new()
        {
            Width = width,
            Height = height,
            MipLevels = 1,
            ArraySize = 1,
            Format = format,
            SampleDesc = new SampleDesc(1, 0),
            Usage = Usage.Staging,
            BindFlags = 0,
            CPUAccessFlags = (uint)CpuAccessFlag.Read,
            MiscFlags = 0,
        };
        ID3D11Texture2D* texture;
        ComInterop.Check(_gpu.CaptureDevice->CreateTexture2D(&desc, null, &texture), "CreateTexture2D (staging)");
        return texture;
    }

    private void DisposeStaging()
    {
        ComInterop.Release(ref _stagingCrop);
        ComInterop.Release(ref _stagingFull);
    }
}
