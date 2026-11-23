using System.Collections.Frozen;
using System.Runtime.InteropServices;

namespace Pstm.Hotkey;

/// A keyboard key or mouse button. The aim is enabled while it is held.
public readonly partial struct Key
{
    private static readonly (string Name, int Code)[] Table =
    [
        ("KeyA", 0x41), ("KeyB", 0x42), ("KeyC", 0x43), ("KeyD", 0x44), ("KeyE", 0x45),
        ("KeyF", 0x46), ("KeyG", 0x47), ("KeyH", 0x48), ("KeyI", 0x49), ("KeyJ", 0x4A),
        ("KeyK", 0x4B), ("KeyL", 0x4C), ("KeyM", 0x4D), ("KeyN", 0x4E), ("KeyO", 0x4F),
        ("KeyP", 0x50), ("KeyQ", 0x51), ("KeyR", 0x52), ("KeyS", 0x53), ("KeyT", 0x54),
        ("KeyU", 0x55), ("KeyV", 0x56), ("KeyW", 0x57), ("KeyX", 0x58), ("KeyY", 0x59),
        ("KeyZ", 0x5A),
        ("Digit0", 0x30), ("Digit1", 0x31), ("Digit2", 0x32), ("Digit3", 0x33), ("Digit4", 0x34),
        ("Digit5", 0x35), ("Digit6", 0x36), ("Digit7", 0x37), ("Digit8", 0x38), ("Digit9", 0x39),
        ("F1", 0x70), ("F2", 0x71), ("F3", 0x72), ("F4", 0x73), ("F5", 0x74), ("F6", 0x75),
        ("F7", 0x76), ("F8", 0x77), ("F9", 0x78), ("F10", 0x79), ("F11", 0x7A), ("F12", 0x7B),
        ("F13", 0x7C), ("F14", 0x7D), ("F15", 0x7E), ("F16", 0x7F), ("F17", 0x80), ("F18", 0x81),
        ("F19", 0x82), ("F20", 0x83), ("F21", 0x84), ("F22", 0x85), ("F23", 0x86), ("F24", 0x87),
        ("ArrowLeft", 0x25), ("ArrowUp", 0x26), ("ArrowRight", 0x27), ("ArrowDown", 0x28),
        ("Backspace", 0x08), ("Enter", 0x0D), ("Space", 0x20), ("Tab", 0x09), ("Insert", 0x2D),
        ("Delete", 0x2E), ("Home", 0x24), ("End", 0x23), ("PageUp", 0x21), ("PageDown", 0x22),
        ("BracketLeft", 0xDB), ("BracketRight", 0xDD), ("Backquote", 0xC0), ("Equal", 0xBB),
        ("Comma", 0xBC), ("Backslash", 0xDC), ("Slash", 0xBF), ("Minus", 0xBD), ("Period", 0xBE),
        ("Semicolon", 0xBA), ("Quote", 0xDE), ("BrowserBack", 0xA6),
        ("MouseLeft", 0x01), ("MouseRight", 0x02), ("MouseMiddle", 0x04),
        ("MouseX1", 0x05), ("MouseX2", 0x06),
    ];

    private const int LeftTrigger = 0x10000;
    private const int RightTrigger = 0x20000;
    private const int TriggerThreshold = 128;

    private static readonly (string Name, int Mask)[] PadTable =
    [
        ("xinput:a", 0x1000), ("xinput:b", 0x2000), ("xinput:x", 0x4000), ("xinput:y", 0x8000),
        ("xinput:lb", 0x0100), ("xinput:rb", 0x0200), ("xinput:back", 0x0020), ("xinput:start", 0x0010),
        ("xinput:left_stick", 0x0040), ("xinput:right_stick", 0x0080),
        ("xinput:dpad_up", 0x0001), ("xinput:dpad_down", 0x0002),
        ("xinput:dpad_left", 0x0004), ("xinput:dpad_right", 0x0008),
        ("xinput:lt", LeftTrigger), ("xinput:rt", RightTrigger),
    ];

    private static readonly FrozenDictionary<string, int> PadMasks =
        PadTable.ToFrozenDictionary(entry => entry.Name, entry => entry.Mask, StringComparer.OrdinalIgnoreCase);

    private static readonly FrozenDictionary<string, int> Codes =
        Table.ToFrozenDictionary(entry => entry.Name, entry => entry.Code, StringComparer.OrdinalIgnoreCase);

    private readonly int _padMask;

    private Key(int virtualKey, int padMask)
    {
        VirtualKey = virtualKey;
        _padMask = padMask;
    }

    /// The Windows virtual-key code of the key or button. The value is zero for a controller input.
    public int VirtualKey { get; }

    /// Parses a key name. The case is ignored. A controller input starts with `xinput:` and a slug,
    /// such as `xinput:a` or `xinput:lt`. A bare letter or digit, such as `A` or `1`, is the same
    /// key as `KeyA` or `Digit1`.
    public static bool TryParse(string name, out Key key)
    {
        if (PadMasks.TryGetValue(name, out var mask))
        {
            key = new Key(0, mask);
            return true;
        }

        var alias = name.Length == 1 && char.IsAsciiLetter(name[0]) ? "Key" + name
            : name.Length == 1 && char.IsAsciiDigit(name[0]) ? "Digit" + name
            : name;
        if (Codes.TryGetValue(alias, out var code))
        {
            key = new Key(code, 0);
            return true;
        }

        key = default;
        return false;
    }

    /// The name of the first key or button that is down now. The value is null when none is down.
    public static string? FirstDown()
    {
        if (XInput.TryRead(out ushort buttons, out byte left, out byte right))
        {
            foreach ((string name, int mask) in PadTable)
            {
                if (PadIsDown(mask, buttons, left, right))
                {
                    return name;
                }
            }
        }

        foreach ((string name, int code) in Table)
        {
            if (IsPressed(code))
            {
                return name;
            }
        }

        return null;
    }

    /// Whether Escape is down now.
    public static bool IsEscapeDown() => IsPressed(0x1B);

    /// Whether the key is down now. The high bit of the result is the down state. The low bit
    /// records a press since the last call. The code does not read it.
    public bool IsDown()
    {
        if (_padMask == 0)
        {
            return IsPressed(VirtualKey);
        }

        return XInput.TryRead(out ushort buttons, out byte left, out byte right)
            && PadIsDown(_padMask, buttons, left, right);
    }

    private static bool IsPressed(int virtualKey) => (GetAsyncKeyState(virtualKey) & 0x8000) != 0;

    [LibraryImport("user32.dll")]
    private static partial short GetAsyncKeyState(int virtualKey);

    private static bool PadIsDown(int mask, ushort buttons, byte left, byte right) => mask switch
    {
        LeftTrigger => left >= TriggerThreshold,
        RightTrigger => right >= TriggerThreshold,
        _ => (buttons & mask) != 0,
    };
}
