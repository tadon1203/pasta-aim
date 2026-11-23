using System.Text.Json.Serialization;

namespace Pstm.Hotkey;

public sealed record HotkeySettings
{
    public const string DefaultKey = "F8";

    private static readonly string[] DefaultKeys = [DefaultKey];

    /// <summary>
    /// The key names that enable the tracker, such as "F8". The tracker is enabled while any of them is down.
    /// </summary>
    [JsonPropertyName("keys")]
    public IReadOnlyList<string> Keys { get; init; } = DefaultKeys;

    /// <summary>Trims and deduplicates the names, ignoring case. Blank names are removed. No name left means the default key.</summary>
    public HotkeySettings Clamped() => this with { Keys = NormalizedKeys(Keys) };

    // The list property makes the generated comparison compare references.
    public bool Equals(HotkeySettings? other) => other is not null && Keys.SequenceEqual(other.Keys);

    public override int GetHashCode()
    {
        HashCode hash = new();
        foreach (string name in Keys)
        {
            hash.Add(name);
        }

        return hash.ToHashCode();
    }

    private static IReadOnlyList<string> NormalizedKeys(IReadOnlyList<string>? names)
    {
        string[] kept = [.. (names ?? []).Where(name => !string.IsNullOrWhiteSpace(name))
            .Select(name => name.Trim())
            .Distinct(StringComparer.OrdinalIgnoreCase)];
        return kept.Length == 0 ? DefaultKeys : kept;
    }
}
