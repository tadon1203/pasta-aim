using System.Text.Json.Serialization;

namespace Pstm.Correction;

public sealed record CorrectionSettings
{
    public const int MinSensitivity = 1;
    public const int MaxSensitivity = 10_000;
    public const int MaxOffset = 100;
    public const int MaxDeltaLimit = 32_767;
    public const int MaxLeadSmoothingMs = 1000;

    [JsonPropertyName("sensitivity_x")]
    public int SensitivityX { get; init; } = 100;

    [JsonPropertyName("sensitivity_y")]
    public int SensitivityY { get; init; } = 100;

    /// <summary>Correction point inside the target box in percent of its height. 0 is the top edge.</summary>
    [JsonPropertyName("offset")]
    public int Offset { get; init; } = 50;

    /// <summary>Per-frame cap on the movement length. 0 means no cap.</summary>
    [JsonPropertyName("max_delta")]
    public int MaxDelta { get; init; } = 512;

    /// <summary>Mouse counts the game needs to move the view by one capture pixel, in percent. 100 is one count.</summary>
    [JsonPropertyName("lead_scale")]
    public int LeadScale { get; init; } = 100;

    /// <summary>Whether the tracker point leads a moving target. Off moves to the box as it is.</summary>
    [JsonPropertyName("lead")]
    public bool Lead { get; init; } = true;

    /// <summary>Time in milliseconds for the velocity estimate to reach 63% of a step change. 0 means no smoothing.</summary>
    [JsonPropertyName("lead_smoothing_ms")]
    public int LeadSmoothingMs { get; init; } = 12;

    /// <summary>Clamps every value into its range.</summary>
    public CorrectionSettings Clamped() => this with
    {
        SensitivityX = Math.Clamp(SensitivityX, MinSensitivity, MaxSensitivity),
        SensitivityY = Math.Clamp(SensitivityY, MinSensitivity, MaxSensitivity),
        Offset = Math.Clamp(Offset, 0, MaxOffset),
        MaxDelta = Math.Clamp(MaxDelta, 0, MaxDeltaLimit),
        LeadScale = Math.Clamp(LeadScale, MinSensitivity, MaxSensitivity),
        LeadSmoothingMs = Math.Clamp(LeadSmoothingMs, 0, MaxLeadSmoothingMs),
    };
}
