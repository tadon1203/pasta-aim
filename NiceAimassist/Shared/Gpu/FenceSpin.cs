using System.Diagnostics;
using Silk.NET.Direct3D12;

namespace NiceAimassist.Shared.Gpu;

internal static unsafe class FenceSpin
{
    private static readonly long Timeout = 5 * Stopwatch.Frequency;

    // A removed device completes every fence with ulong.MaxValue. The check then returns.
    public static void Wait(ID3D12Fence* fence, ulong value)
    {
        if (fence->GetCompletedValue() >= value)
        {
            return;
        }

        long deadline = Stopwatch.GetTimestamp() + Timeout;
        while (fence->GetCompletedValue() < value)
        {
            if (Stopwatch.GetTimestamp() > deadline)
            {
                throw new TimeoutException($"the GPU fence did not reach {value} in 5 s");
            }

            Thread.SpinWait(1);
        }
    }
}
