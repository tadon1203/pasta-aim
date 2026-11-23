using System.Text.Json.Serialization;

namespace Pstm.Detect;

public sealed record DetectSettings
{
    public const int MaxThreshold = 100;

    /// <summary>The .onnx file name inside models/ next to the executable. The value is null when no model is chosen.</summary>
    [JsonPropertyName("model")]
    public string? Model { get; init; }

    /// <summary>Minimum detection threshold in percent.</summary>
    [JsonPropertyName("threshold")]
    public int Threshold { get; init; } = 50;

    /// <summary>Clamps the threshold into its range. Replaces an empty model name with null.</summary>
    public DetectSettings Clamped() => this with
    {
        Model = string.IsNullOrWhiteSpace(Model) ? null : Model,
        Threshold = Math.Clamp(Threshold, 0, MaxThreshold),
    };
}
