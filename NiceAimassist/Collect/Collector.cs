using System.Buffers;
using NiceAimassist.Capture;
using NiceAimassist.Display;
using Silk.NET.Direct3D11;
using Silk.NET.DXGI;
using D3D11Box = Silk.NET.Direct3D11.Box;

namespace NiceAimassist.Collect;

/// <summary>
/// Copies the desktop into staging textures. Reads them back later. The loop thread never waits on the GPU.
/// Every method except the readers runs on the loop thread. That thread owns the immediate context.
/// </summary>
public sealed unsafe class Collector : ICollectControl, IFrameObserver, IDisposable
{
    private const uint DoNotWait = 0x100000;

    private readonly DisplayDevices _gpu;
    private readonly CollectSchedule _schedule;
    private readonly SampleWriter _writer;
    private readonly DatasetWriter _dataset;
    private volatile bool _active;
    private volatile string? _lastError;
    private int _size = 640;
    private ID3D11Texture2D* _stagingCrop;
    private ID3D11Texture2D* _stagingFull;
    private bool _pending;
    private bool _failed;
    private string _pendingFileName = string.Empty;
    private bool _disposed;

    public Collector(DisplayDevices gpu, string datasetRoot, Func<long> now, long ticksPerSecond)
    {
        _gpu = gpu;
        _dataset = new DatasetWriter(datasetRoot);
        _schedule = new CollectSchedule(now, ticksPerSecond);
        _writer = new SampleWriter(Write, error => _lastError = error.Message);
    }

    public bool Active => _active;

    public long Saved => _writer.Saved;

    public string? LastError => _lastError;

    public bool IsDue => _active && !_pending && _schedule.IsDue;

    /// <summary>
    /// Sets whether collection runs. The loop thread calls it on every loop. A failure stays latched
    /// until <paramref name="enabled"/> is false. A persistent error then does not restart on every frame.
    /// </summary>
    public void SetEnabled(bool enabled)
    {
        if (!enabled)
        {
            _active = false;
            _pending = false;
            _failed = false;
            return;
        }

        if (_active || _failed)
        {
            return;
        }

        _lastError = null;
        _schedule.Restart();
        _active = true;
    }

    public void Configure(int size, int perSecond)
    {
        _size = size;
        _schedule.SetRate(perSecond);
    }

    public void Observe(ID3D11Texture2D* desktop)
    {
        try
        {
            Texture2DDesc desc;
            desktop->GetDesc(&desc);
            (_, _, int side) = CenterCrop.Square((int)desc.Width, (int)desc.Height, _size);
            EnsureStaging(desc, side);

            ID3D11DeviceContext* context = _gpu.CaptureContext;
            D3D11Box box = CenterCrop.Box((int)desc.Width, (int)desc.Height, side, side);
            context->CopySubresourceRegion((ID3D11Resource*)_stagingCrop, 0, 0, 0, 0, (ID3D11Resource*)desktop, 0, &box);
            // The full copy goes last. When it is readable, the crop copy is readable too.
            context->CopyResource((ID3D11Resource*)_stagingFull, (ID3D11Resource*)desktop);
            // Without a flush, a DoNotWait map can report "still drawing" until the driver flushes.
            context->Flush();
            _pendingFileName = DatasetWriter.FileNameFor(DateTime.Now);
            _pending = true;
            _schedule.MarkTaken();
        }
        catch (Exception error)
        {
            Fail(error);
        }
    }

    public void SubmitReadySample()
    {
        if (!_pending)
        {
            return;
        }

        try
        {
            if (!TryRead(_stagingFull, wait: false, out BgraImage original))
            {
                return;
            }

            _pending = false;
            if (!TryRead(_stagingCrop, wait: true, out BgraImage crop))
            {
                Release(original);
                Fail(new InvalidOperationException("the crop staging texture was not readable"));
                return;
            }

            if (!_writer.TrySubmit(new Sample(_pendingFileName, crop, original)))
            {
                Release(crop);
                Release(original);
            }
        }
        catch (Exception error)
        {
            Fail(error);
        }
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        _active = false;
        _writer.Dispose();
        DisposeStaging();
    }

    private static void Release(BgraImage image) => ArrayPool<byte>.Shared.Return(image.Pixels);

    private void Write(Sample sample)
    {
        try
        {
            _dataset.Write(sample.FileNameWithoutExtension, sample.Crop, sample.Original);
        }
        finally
        {
            Release(sample.Crop);
            Release(sample.Original);
        }
    }

    private void Fail(Exception error)
    {
        _lastError = error.Message;
        _active = false;
        _pending = false;
        _failed = true;
    }

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
