namespace NiceAimassist.Hold;

/// <summary>The hold keys. The loop thread reads them. The settings change replaces them with <see cref="Update"/>.</summary>
public sealed class HoldKeySet
{
    private HoldBinding _binding;

    /// <exception cref="InvalidOperationException">A name is not a known key.</exception>
    public HoldKeySet(IReadOnlyList<string> names)
    {
        foreach (string name in names)
        {
            if (!HoldKey.TryParse(name, out _))
            {
                throw new InvalidOperationException($"unknown hold key \"{name}\" in settings.json");
            }
        }

        _binding = new HoldBinding(names, Parse(names));
    }

    internal HoldKey[] ActiveKeys => Volatile.Read(ref _binding).Keys;

    public bool IsHeld()
    {
        foreach (HoldKey key in Volatile.Read(ref _binding).Keys)
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
        HoldBinding current = Volatile.Read(ref _binding);
        if (current.Names.SequenceEqual(names))
        {
            return;
        }

        HoldKey[] keys = Parse(names);
        if (keys.Length == 0)
        {
            Volatile.Write(ref _binding, current with { Names = names });
            Log.Warn("no known hold key; keeping the previous keys");
            return;
        }

        if (keys.Length < names.Count)
        {
            Log.Warn("unknown hold keys are ignored");
        }

        Volatile.Write(ref _binding, new HoldBinding(names, keys));
    }

    private static HoldKey[] Parse(IReadOnlyList<string> names) =>
        [.. names.Select(name => HoldKey.TryParse(name, out HoldKey key) ? (HoldKey?)key : null)
            .OfType<HoldKey>()];

    private sealed record HoldBinding(IReadOnlyList<string> Names, HoldKey[] Keys);
}
