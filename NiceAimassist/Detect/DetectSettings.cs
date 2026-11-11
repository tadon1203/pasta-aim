using System.Text.Json.Serialization;

namespace NiceAimassist.Detect;

public sealed record DetectSettings
{
    public const int MaxConfidence = 100;

    /// <summary>The .onnx file name inside models/ next to the executable. The value is null when no model is chosen.</summary>
    [JsonPropertyName("model")]
    public string? Model { get; init; }

    /// <summary>Minimum detection confidence in percent.</summary>
    [JsonPropertyName("confidence")]
    public int Confidence { get; init; } = 50;

    /// <summary>Clamps the confidence into its range. Replaces an empty model name with null.</summary>
    public DetectSettings Clamped() => this with
    {
        Model = string.IsNullOrWhiteSpace(Model) ? null : Model,
        Confidence = Math.Clamp(Confidence, 0, MaxConfidence),
    };
}
