using System.Text.Json.Serialization;

namespace NiceAimassist.Mouse;

public sealed record MouseSettings
{
    [JsonPropertyName("interpolation")]
    public Interpolation Interpolation { get; init; }
}
