using System.Text.Json.Serialization;

namespace NiceAimassist.Dataset;

public sealed record DatasetSettings
{
    /// <summary>Whether recording runs while a hotkey is down.</summary>
    [JsonPropertyName("enabled")]
    public bool Enabled { get; init; } = true;
}
