using NiceAimassist.Capture;
using NiceAimassist.Shared.Gpu;
using NiceAimassist.Detect.Model;
using NiceAimassist.Detect.Pipeline;
using NiceAimassist.Detect.Runtime;
using Silk.NET.Direct3D12;
using Silk.NET.DXGI;

namespace NiceAimassist.Tests.Detect.Pipeline;

public unsafe class PreprocessorGpuTests
{
    private const int Size = 64;

    [Fact(Skip = "needs an interactive desktop session and a GPU")]
    public void PreprocessWritesTheCapturedFrameIntoTheTensor()
    {
        using GpuDevices gpu = new(0, 0);
        using DesktopCapture frames = new(gpu, Size, Size);
        int bytes = 3 * Size * Size * sizeof(float);

        HeapProperties heap = new() { Type = HeapType.Default, CreationNodeMask = 1, VisibleNodeMask = 1 };
        ResourceDesc description = new()
        {
            Dimension = ResourceDimension.Buffer,
            Width = (ulong)bytes,
            Height = 1,
            DepthOrArraySize = 1,
            MipLevels = 1,
            Format = Format.FormatUnknown,
            SampleDesc = new SampleDesc(1, 0),
            Layout = TextureLayout.LayoutRowMajor,
            Flags = ResourceFlags.AllowUnorderedAccess,
        };
        ID3D12Resource* tensor;
        ComInterop.Check(
            gpu.Device->CreateCommittedResource(
                &heap, HeapFlags.None, &description, ResourceStates.UnorderedAccess, null, ComInterop.IidOf<ID3D12Resource>(), (void**)&tensor),
            "CreateCommittedResource");
        try
        {
            using GpuTimer timer = new(gpu);
            using Preprocessor preprocessor = new(gpu, frames, TensorPrecision.Fp32, timer);
            using OutputReader reader = new(gpu, tensor, bytes, timer);

            CapturedFrame frame = default;
            bool acquired = false;
            for (int attempt = 0; attempt < 100 && !acquired; attempt++)
            {
                acquired = frames.TryAcquire(50, out frame) == CaptureStatus.Ready;
            }

            Assert.True(acquired, "no desktop frame");
            preprocessor.Submit(frame, tensor);
            float[] values = System.Runtime.InteropServices.MemoryMarshal.Cast<byte, float>(reader.Read()).ToArray();

            // A desktop is never all black. Each channel needs at least one non-zero value.
            int pixels = Size * Size;
            for (int channel = 0; channel < 3; channel++)
            {
                Assert.True(values.AsSpan(channel * pixels, pixels).ContainsAnyExcept(0f), $"channel {channel} is all zero");
            }
        }
        finally
        {
            gpu.WaitIdle();
            ComInterop.Release(ref tensor);
        }
    }
}
