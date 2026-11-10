using System.Runtime.InteropServices;
using Silk.NET.Direct3D12;

namespace NiceAimassist.Gpu;

internal static unsafe partial class NativeGpu
{
    [LibraryImport("kernel32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool CloseHandle(nint handle);

    internal static void CloseSharedHandle(void* handle) => CloseHandle((nint)handle);

    // Spins instead of sleeping. A removed device completes every fence with ulong.MaxValue, which ends the loop.
    internal static void SpinUntil(ID3D12Fence* fence, ulong value)
    {
        while (fence->GetCompletedValue() < value)
        {
            Thread.SpinWait(64);
        }
    }
}
