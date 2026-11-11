using System.Text.Json.Serialization;

namespace NiceAimassist.Hold;

public sealed record HoldSettings
{
    public const string DefaultKey = "F8";

    private static readonly string[] DefaultKeys = [DefaultKey];

    /// <summary>
    /// The key names that hold the aim, such as "F8". The aim is held while any of them is down.
    /// </summary>
    [JsonPropertyName("keys")]
    public IReadOnlyList<string> Keys { get; init; } = DefaultKeys;

    /// <summary>Trims and deduplicates the names, ignoring case. Blank names are removed. No name left means the default key.</summary>
    public HoldSettings Clamped() => this with { Keys = NormalizedKeys(Keys) };

    private static IReadOnlyList<string> NormalizedKeys(IReadOnlyList<string>? names)
    {
        string[] kept = [.. (names ?? []).Where(name => !string.IsNullOrWhiteSpace(name))
            .Select(name => name.Trim())
            .Distinct(StringComparer.OrdinalIgnoreCase)];
        return kept.Length == 0 ? DefaultKeys : kept;
    }
}
