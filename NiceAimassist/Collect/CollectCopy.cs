using System.Buffers;
using NiceAimassist.Capture;
using NiceAimassist.Display;
using Silk.NET.Direct3D11;
using Silk.NET.DXGI;
using D3D11Box = Silk.NET.Direct3D11.Box;

namespace NiceAimassist.Collect;

// Every method runs on the loop thread. That thread owns the immediate context.
internal sealed unsafe class CollectCopy : IDisposable
{
    private const uint DoNotWait = 0x100000;

    private readonly DisplayDevices _gpu;
    private ID3D11Texture2D* _stagingCrop;
    private ID3D11Texture2D* _stagingFull;

    public CollectCopy(DisplayDevices gpu) => _gpu = gpu;

    /// <summary>Copies the centered square of <paramref name="size"/> px and the full desktop into the staging textures.</summary>
    public void Copy(ID3D11Texture2D* desktop, int size)
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

    /// <summary>Reads the full desktop copy without waiting. Returns false while the GPU still draws it.</summary>
    public bool TryReadFull(out BgraImage image) => TryRead(_stagingFull, wait: false, out image);

    public bool TryReadCrop(out BgraImage image) => TryRead(_stagingCrop, wait: true, out image);

    public void Dispose() => DisposeStaging();

    private bool TryRead(ID3D11Texture2D* staging, bool wait, out BgraImage image)
    {
        image = default;
        ID3D11DeviceContext* context = _gpu.CaptureContext;
        MappedSubresource mapped;
        int result = context->Map((ID3D11Resource*)staging, 0, Map.Read, wait ? 0 : DoNotWait, &mapped);
        if (result == Com.DxgiWasStillDrawing)
        {
            return false;
        }

        Com.Check(result, "Map the staging texture");
        try
        {
            Texture2DDesc desc;
            staging->GetDesc(&desc);
            int width = (int)desc.Width;
            int height = (int)desc.Height;
            int rowBytes = width * 4;
            byte[] pixels = ArrayPool<byte>.Shared.Rent(rowBytes * height);
            for (int row = 0; row < height; row++)
            {
                new ReadOnlySpan<byte>((byte*)mapped.PData + (long)row * mapped.RowPitch, rowBytes).CopyTo(pixels.AsSpan(row * rowBytes));
            }

            image = new BgraImage(pixels, width, height);
            return true;
        }
        finally
        {
            context->Unmap((ID3D11Resource*)staging, 0);
        }
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
            Com.Release(ref newFull);
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
        Com.Check(_gpu.CaptureDevice->CreateTexture2D(&desc, null, &texture), "CreateTexture2D (staging)");
        return texture;
    }

    private void DisposeStaging()
    {
        Com.Release(ref _stagingCrop);
        Com.Release(ref _stagingFull);
    }
}
