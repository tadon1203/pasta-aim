using System.Text.Json.Serialization;

namespace NiceAimassist.Aim;

public sealed record AimSettings
{
    public const int MinSensitivity = 1;
    public const int MaxSensitivity = 10_000;
    public const int MaxTargetYOffset = 100;
    public const int MaxDeltaLimit = 32_767;
    public const int MaxVelocitySmoothing = 95;

    [JsonPropertyName("sensitivity_x")]
    public int SensitivityX { get; init; } = 100;

    [JsonPropertyName("sensitivity_y")]
    public int SensitivityY { get; init; } = 100;

    /// <summary>Aim point inside the target box in percent of its height. 0 is the top edge.</summary>
    [JsonPropertyName("target_y_offset")]
    public int TargetYOffset { get; init; } = 50;

    /// <summary>Per-frame cap on the movement length. 0 means no cap.</summary>
    [JsonPropertyName("max_delta")]
    public int MaxDelta { get; init; } = 512;

    [JsonPropertyName("velocity_smoothing")]
    public int VelocitySmoothing { get; init; } = 50;

    /// <summary>Clamps every value into its range.</summary>
    public AimSettings Clamped() => this with
    {
        SensitivityX = Math.Clamp(SensitivityX, MinSensitivity, MaxSensitivity),
        SensitivityY = Math.Clamp(SensitivityY, MinSensitivity, MaxSensitivity),
        TargetYOffset = Math.Clamp(TargetYOffset, 0, MaxTargetYOffset),
        MaxDelta = Math.Clamp(MaxDelta, 0, MaxDeltaLimit),
        VelocitySmoothing = Math.Clamp(VelocitySmoothing, 0, MaxVelocitySmoothing),
    };
}
