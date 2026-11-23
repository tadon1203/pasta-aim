using System.Text.Json.Serialization;

namespace Pstm.Mouse;

public sealed record MouseSettings
{
    [JsonPropertyName("interpolation")]
    public Interpolation Interpolation { get; init; }
}
