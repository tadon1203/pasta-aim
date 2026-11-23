using System.Text.Json.Serialization;

namespace Pstm.Mouse;

public sealed record MouseSettings
{
    [JsonPropertyName("smoothing")]
    public Smoothing Smoothing { get; init; }
}
