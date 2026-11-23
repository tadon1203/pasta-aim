using System.Text.Json.Serialization;

namespace Pstm.Samples;

public sealed record SampleSettings
{
    /// <summary>Whether recording runs while a hotkey is down.</summary>
    [JsonPropertyName("enabled")]
    public bool Enabled { get; init; } = true;
}
