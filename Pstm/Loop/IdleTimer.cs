using System.Runtime.InteropServices;

namespace Pstm.Loop;

// Create and dispose the timer on the loop thread.
internal sealed partial class IdleTimer : IDisposable
{
    private const long HundredNanosecondsPerMs = 10_000;
    private const uint CreateWaitableTimerHighResolution = 0x2;
    private const uint TimerAllAccess = 0x1F0003;
    private const uint Infinite = 0xFFFFFFFF;

    private nint _handle;

    public IdleTimer()
    {
        _handle = CreateWaitableTimerExW(0, 0, CreateWaitableTimerHighResolution, TimerAllAccess);
        if (_handle == 0)
        {
            throw new InvalidOperationException($"CreateWaitableTimerExW failed ({Marshal.GetLastPInvokeError()})");
        }
    }

    public void SleepMs(int milliseconds)
    {
        // A negative due time is relative. The unit is 100 ns.
        long dueTime = -(long)milliseconds * HundredNanosecondsPerMs;
        if (!SetWaitableTimer(_handle, in dueTime, 0, 0, 0, false))
        {
            throw new InvalidOperationException($"SetWaitableTimer failed ({Marshal.GetLastPInvokeError()})");
        }

        if (WaitForSingleObject(_handle, Infinite) != 0)
        {
            throw new InvalidOperationException($"WaitForSingleObject failed ({Marshal.GetLastPInvokeError()})");
        }
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
    private static partial nint CreateWaitableTimerExW(nint attributes, nint name, uint flags, uint access);

    [LibraryImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SetWaitableTimer(
        nint timer,
        in long dueTime,
        int period,
        nint completion,
        nint arg,
        [MarshalAs(UnmanagedType.Bool)] bool resume);

    [LibraryImport("kernel32.dll", SetLastError = true)]
    private static partial uint WaitForSingleObject(nint handle, uint milliseconds);

    [LibraryImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool CloseHandle(nint handle);
}
