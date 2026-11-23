using Microsoft.Win32;

namespace Pstm.Mouse.Serial;

internal static class DevicePorts
{
    private static readonly (int Vid, int Pid)[] Chips = [(0x1A86, 0x55D3), (0x1A86, 0x7523)];

    /// <summary>COM ports of the CH340 chips that the device uses. The ports are read from the USB device registry.</summary>
    public static IReadOnlyList<string> Candidates()
    {
        var ports = new List<string>();
        foreach ((int vid, int pid) in Chips)
        {
            using RegistryKey? chip = Registry.LocalMachine.OpenSubKey(
                $@"SYSTEM\CurrentControlSet\Enum\USB\VID_{vid:X4}&PID_{pid:X4}");
            if (chip is null)
            {
                continue;
            }

            foreach (string instance in chip.GetSubKeyNames())
            {
                using RegistryKey? parameters = chip.OpenSubKey($@"{instance}\Device Parameters");
                if (parameters?.GetValue("PortName") is string port && port.Length > 0)
                {
                    ports.Add(port);
                }
            }
        }

        return ports;
    }
}
