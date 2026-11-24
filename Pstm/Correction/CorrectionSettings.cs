using System.Text.Json.Serialization;

namespace Pstm.Correction;

public sealed record CorrectionSettings
{
    public const int MinSensitivity = 1;
    public const int MaxSensitivity = 10_000;
    public const int MaxOffset = 100;
    public const int MaxDeltaLimit = 32_767;
    public const int MaxNearPx = 1000;
    public const int MaxNearBoost = 10_000;

    [JsonPropertyName("sensitivity_x")]
    public int SensitivityX { get; init; } = 5;

    [JsonPropertyName("sensitivity_y")]
    public int SensitivityY { get; init; } = 5;

    /// <summary>Correction point inside the target box in percent of its height. 0 is the top edge.</summary>
    [JsonPropertyName("offset")]
    public int Offset { get; init; } = 50;

    /// <summary>Per-frame cap on the movement length. 0 means no cap.</summary>
    [JsonPropertyName("max_delta")]
    public int MaxDelta { get; init; } = 512;

    /// <summary>Distance in input pixels where the near boost starts. 0 disables the boost.</summary>
    [JsonPropertyName("near_px")]
    public int NearPx { get; init; } = 128;

    /// <summary>Sensitivity added at the target point. 0 keeps the base gain.</summary>
    [JsonPropertyName("near_boost")]
    public int NearBoost { get; init; } = 10;

    /// <summary>Clamps every value into its range.</summary>
    public CorrectionSettings Clamped() => this with
    {
        SensitivityX = Math.Clamp(SensitivityX, MinSensitivity, MaxSensitivity),
        SensitivityY = Math.Clamp(SensitivityY, MinSensitivity, MaxSensitivity),
        Offset = Math.Clamp(Offset, 0, MaxOffset),
        MaxDelta = Math.Clamp(MaxDelta, 0, MaxDeltaLimit),
        NearPx = Math.Clamp(NearPx, 0, MaxNearPx),
        NearBoost = Math.Clamp(NearBoost, 0, MaxNearBoost),
    };
}
