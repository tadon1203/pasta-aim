using NiceAimassist.Shared.Logging;

namespace NiceAimassist.Hotkey;

/// <summary>The hotkeys. The loop thread reads them. The settings change replaces them with <see cref="Update"/>.</summary>
public sealed class HotkeySet
{
    private Key[] _keys = [];

    /// <summary>Sets the keys with <see cref="Update"/>. Check <see cref="HasKeys"/> when the set must not start empty.</summary>
    public HotkeySet(IReadOnlyList<string> names) => Update(names);

    public bool HasKeys => ActiveKeys.Length > 0;

    internal Key[] ActiveKeys => Volatile.Read(ref _keys);

    public bool IsHeld()
    {
        foreach (Key key in Volatile.Read(ref _keys))
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
        List<Key> keys = [];
        List<string> unknown = [];
        foreach (string name in names)
        {
            if (Key.TryParse(name, out Key key))
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
            return;
        }

        Volatile.Write(ref _keys, [.. keys]);
    }
}
