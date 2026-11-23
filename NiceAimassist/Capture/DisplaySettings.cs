using System.Text.Json.Serialization;

namespace NiceAimassist.Capture;

public sealed record DisplaySettings
{
    /// <summary>The DirectX adapter LUID that capture uses. 0 selects the first adapter.</summary>
    [JsonPropertyName("adapter")]
    public long Adapter { get; init; }

    /// <summary>The display output index on the adapter.</summary>
    [JsonPropertyName("output")]
    public int Output { get; init; }
}
