using System.Runtime.InteropServices;
using NiceAimassist.Shared.Logging;

namespace NiceAimassist.Loop;

internal static partial class LoopThreadBoost
{
    /// <summary>Raises the priority of the calling thread. A failure of MMCSS only logs a warning.</summary>
    public static void Apply()
    {
        Thread.CurrentThread.Priority = ThreadPriority.Highest;
        uint taskIndex = 0;
        if (AvSetMmThreadCharacteristicsW("Games", ref taskIndex) == 0)
        {
            Log.Warn($"MMCSS registration of the loop thread failed ({Marshal.GetLastPInvokeError()})");
        }
    }

    [LibraryImport("avrt.dll", StringMarshalling = StringMarshalling.Utf16, SetLastError = true)]
    private static partial nint AvSetMmThreadCharacteristicsW(string taskName, ref uint taskIndex);
}
