using System.Runtime.InteropServices;

namespace Pstm.Hotkey;

/// Reads the first connected XInput controller. A disconnected slot is slow to query. The slots
/// are scanned again only once a second while no controller is connected.
internal static partial class XInput
{
    private const int Slots = 4;
    private const long RescanTicks = 1_000;

    private static int _slot = -1;
    private static long _nextScan;

    public static bool TryRead(out ushort buttons, out byte left, out byte right)
    {
        int slot = Volatile.Read(ref _slot);
        if (slot >= 0)
        {
            if (GetState(slot, out State state) == 0)
            {
                (buttons, left, right) = (state.Buttons, state.LeftTrigger, state.RightTrigger);
                return true;
            }

            Volatile.Write(ref _slot, -1);
        }

        long now = Environment.TickCount64;
        if (now >= Volatile.Read(ref _nextScan))
        {
            Volatile.Write(ref _nextScan, now + RescanTicks);
            for (int index = 0; index < Slots; index++)
            {
                if (GetState(index, out State state) == 0)
                {
                    Volatile.Write(ref _slot, index);
                    (buttons, left, right) = (state.Buttons, state.LeftTrigger, state.RightTrigger);
                    return true;
                }
            }
        }

        (buttons, left, right) = (0, 0, 0);
        return false;
    }

    [LibraryImport("xinput1_4.dll", EntryPoint = "XInputGetState")]
    private static partial int GetState(int userIndex, out State state);

    [StructLayout(LayoutKind.Sequential)]
    private struct State
    {
        public uint PacketNumber;
        public ushort Buttons;
        public byte LeftTrigger;
        public byte RightTrigger;
        public short ThumbLX;
        public short ThumbLY;
        public short ThumbRX;
        public short ThumbRY;
    }
}
