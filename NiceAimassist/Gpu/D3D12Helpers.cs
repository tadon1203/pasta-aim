using System.Text;
using Silk.NET.Core.Native;
using Silk.NET.Direct3D.Compilers;
using Silk.NET.Direct3D12;

namespace NiceAimassist.Gpu;

internal static unsafe class D3D12Helpers
{
    // D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES: the barrier covers every mip level and array slice.
    private const uint AllSubresources = uint.MaxValue;

    /// <summary>A whole-resource transition barrier from <paramref name="before"/> to <paramref name="after"/>.</summary>
    internal static ResourceBarrier Transition(ID3D12Resource* resource, ResourceStates before, ResourceStates after)
    {
        ResourceBarrier barrier = new() { Type = ResourceBarrierType.Transition, Flags = ResourceBarrierFlags.None };
        barrier.Anonymous.Transition = new ResourceTransitionBarrier
        {
            PResource = resource,
            Subresource = AllSubresources,
            StateBefore = before,
            StateAfter = after,
        };
        return barrier;
    }

    /// <summary>The text of a compiler or serializer error blob, or an empty string when there is none.</summary>
    internal static string BlobToString(ID3D10Blob* blob)
    {
        if (blob == null)
        {
            return string.Empty;
        }

        // SAFETY: the blob is non-null and owned by the caller until it releases it after this call. GetBufferPointer
        // and GetBufferSize describe the blob's own buffer, which stays valid until the blob is released.
        return Encoding.UTF8.GetString((byte*)blob->GetBufferPointer(), (int)blob->GetBufferSize());
    }
}
