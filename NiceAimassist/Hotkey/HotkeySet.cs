using NiceAimassist.Shared.Logging;

namespace NiceAimassist.Hotkey;

/// <summary>The hotkeys. The loop thread reads them. The settings change replaces them with <see cref="Update"/>.</summary>
public sealed class HotkeySet
{
    private HotkeyBinding _binding = new([], []);

    /// <summary>Sets the keys with <see cref="Update"/>. Check <see cref="HasKeys"/> when the set must not start empty.</summary>
    public HotkeySet(IReadOnlyList<string> names) => Update(names);

    public bool HasKeys => ActiveKeys.Length > 0;

    internal KeyInput[] ActiveKeys => Volatile.Read(ref _binding).Keys;

    public bool IsHeld()
    {
        foreach (KeyInput key in Volatile.Read(ref _binding).Keys)
        {
            if (key.IsDown())
            {
                return true;
            }
        }

        return false;
    }

    /// <summary>Replaces the keys. Unknown names are ignored. When no name is known, the previous keys stay.</summary>
    public void Update(IReadOnlyList<string> names)
    {
        HotkeyBinding current = Volatile.Read(ref _binding);
        if (current.Names.SequenceEqual(names))
        {
            return;
        }

        List<KeyInput> keys = [];
        List<string> unknown = [];
        foreach (string name in names)
        {
            if (KeyInput.TryParse(name, out KeyInput key))
            {
                keys.Add(key);
            }
            else
            {
                unknown.Add(name);
            }
        }

        if (unknown.Count > 0)
        {
            Log.Warn($"unknown hotkeys are ignored: {string.Join(", ", unknown)}");
        }

        if (keys.Count == 0)
        {
            Volatile.Write(ref _binding, current with { Names = names });
            return;
        }

        Volatile.Write(ref _binding, new HotkeyBinding(names, [.. keys]));
    }

    private sealed record HotkeyBinding(IReadOnlyList<string> Names, KeyInput[] Keys);
}
