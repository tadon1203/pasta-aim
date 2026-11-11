using Silk.NET.Direct3D12;

namespace NiceAimassist.Display;

internal static unsafe class D3D12Barrier
{
    // D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES. The barrier covers every mip level and array slice.
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
}
