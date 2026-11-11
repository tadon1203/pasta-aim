using System.Runtime.InteropServices;
using Silk.NET.Direct3D12;

namespace NiceAimassist.Shared.Gpu;

// The event is auto-reset. Only one thread waits on one FenceEvent at a time.
internal sealed unsafe partial class FenceEvent : IDisposable
{
    private const uint WaitObject0 = 0;
    private const uint WaitTimeout = 0x102;
    private const uint TimeoutMilliseconds = 5000;

    private nint _handle;

    public FenceEvent()
    {
        _handle = CreateEventW(0, false, false, 0);
        if (_handle == 0)
        {
            throw new InvalidOperationException($"CreateEventW failed (error 0x{Marshal.GetLastPInvokeError():X8}).");
        }
    }

    public static void WaitOnce(ID3D12Fence* fence, ulong value)
    {
        using FenceEvent waiter = new();
        waiter.Wait(fence, value);
    }

    // A removed device completes every fence with ulong.MaxValue. The first check then returns, so no wait starts.
    public void Wait(ID3D12Fence* fence, ulong value)
    {
        ObjectDisposedException.ThrowIf(_handle == 0, this);
        if (fence->GetCompletedValue() >= value)
        {
            return;
        }

        ComInterop.Check(fence->SetEventOnCompletion(value, (void*)_handle), "SetEventOnCompletion");
        uint result = WaitForSingleObject(_handle, TimeoutMilliseconds);
        if (result == WaitObject0)
        {
            return;
        }

        if (result == WaitTimeout)
        {
            throw new TimeoutException($"the GPU fence did not reach {value} in 5 s");
        }

        throw new InvalidOperationException($"WaitForSingleObject failed (result 0x{result:X8}) while it waited for the GPU fence.");
    }

    public void Dispose()
    {
        if (_handle == 0)
        {
            return;
        }

        CloseHandle(_handle);
        _handle = 0;
    }

    [LibraryImport("kernel32.dll", SetLastError = true)]
    private static partial nint CreateEventW(
        nint attributes,
        [MarshalAs(UnmanagedType.Bool)] bool manualReset,
        [MarshalAs(UnmanagedType.Bool)] bool initialState,
        nint name);

    [LibraryImport("kernel32.dll", SetLastError = true)]
    private static partial uint WaitForSingleObject(nint handle, uint milliseconds);

    [LibraryImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool CloseHandle(nint handle);
}
