using NiceAimassist.Capture;
using NiceAimassist.Shared.Gpu;
using Silk.NET.Direct3D11;
using Silk.NET.DXGI;

namespace NiceAimassist.Dataset.Staging;

// Every method except MarkCopied runs on the loop thread. That thread owns the immediate context.
internal sealed unsafe class StagingTexture : IDisposable
{
    private const uint DoNotWait = 0x100000;
    private const int DxgiWasStillDrawing = unchecked((int)0x887A000A);

    private readonly GpuDevices _gpu;
    private readonly ID3D11DeviceContext* _context;
    private readonly StagingGate _gate = new();
    private ID3D11Texture2D* _staging;

    public StagingTexture(GpuDevices gpu)
    {
        _gpu = gpu;
        _context = (ID3D11DeviceContext*)gpu.CaptureContext;
    }

    public bool CanCopy => _gate.CanCopy;

    public bool IsPending => _gate.IsPending;

    /// <summary>Copies the full desktop into the staging texture.</summary>
    public void Copy(ID3D11Texture2D* desktop)
    {
        // This path uses no fence. The source and the staging texture belong to the capture context,
        // so one copy runs at a time.
        _gate.BeginCopy();
        try
        {
            Texture2DDesc desc;
            desktop->GetDesc(&desc);
            EnsureStaging(desc);

            ID3D11DeviceContext* context = _context;
            context->CopyResource((ID3D11Resource*)_staging, (ID3D11Resource*)desktop);
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
    /// Maps the copy without waiting. Returns false while the GPU still draws it.
    /// The texture stays mapped until <see cref="ReleaseIfCopied"/> or <see cref="CancelMap"/>.
    /// </summary>
    public bool TryMap(out MappedPixels pixels)
    {
        pixels = default;
        if (!_gate.IsPending)
        {
            throw new InvalidOperationException("Map the staging texture: no copy is pending");
        }

        ID3D11DeviceContext* context = _context;
        MappedSubresource mapped;
        int result = context->Map((ID3D11Resource*)_staging, 0, Map.Read, DoNotWait, &mapped);
        if (result == DxgiWasStillDrawing)
        {
            return false;
        }

        ComInterop.Check(result, "Map the staging texture");
        _gate.Map();
        Texture2DDesc desc;
        _staging->GetDesc(&desc);
        pixels = new MappedPixels((nint)mapped.PData, (int)mapped.RowPitch, (int)desc.Width, (int)desc.Height);
        return true;
    }

    /// <summary>Called on the writer thread after it copied the mapped rows.</summary>
    public void MarkCopied() => _gate.MarkCopied();

    /// <summary>Unmaps the texture once the writer copied it. Does nothing before that.</summary>
    public void ReleaseIfCopied()
    {
        if (!_gate.IsCopied)
        {
            return;
        }

        Unmap();
        _gate.Release();
    }

    /// <summary>Unmaps the texture when the writer never receives the mapped sample.</summary>
    public void CancelMap()
    {
        Unmap();
        _gate.CancelMap();
    }

    // Stopping the writer comes first. The writer must not read memory that this call unmaps.
    public void Dispose()
    {
        if (_gate.IsMapped)
        {
            Unmap();
        }

        ComInterop.Release(ref _staging);
    }

    private void Unmap() => _context->Unmap((ID3D11Resource*)_staging, 0);

    // A failed rebuild keeps the old texture. It still matches the old size.
    private void EnsureStaging(Texture2DDesc desktop)
    {
        if (_staging != null)
        {
            Texture2DDesc current;
            _staging->GetDesc(&current);
            if (current.Width == desktop.Width && current.Height == desktop.Height)
            {
                return;
            }
        }

        Texture2DDesc desc = new()
        {
            Width = desktop.Width,
            Height = desktop.Height,
            MipLevels = 1,
            ArraySize = 1,
            Format = desktop.Format,
            SampleDesc = new SampleDesc(1, 0),
            Usage = Usage.Staging,
            BindFlags = 0,
            CPUAccessFlags = (uint)CpuAccessFlag.Read,
            MiscFlags = 0,
        };
        ID3D11Texture2D* created;
        ComInterop.Check(_gpu.CaptureDevice->CreateTexture2D(&desc, null, &created), "CreateTexture2D (staging)");
        ComInterop.Release(ref _staging);
        _staging = created;
    }
}
