using System.Text.Json.Serialization;

namespace NiceAimassist.Aim;

public sealed record AimSettings
{
    public const int MinSensitivity = 1;
    public const int MaxSensitivity = 10_000;
    public const int MaxTargetYOffset = 100;
    public const int MaxDeltaLimit = 32_767;
    public const int MaxVelocitySmoothingMs = 1000;

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

    /// <summary>Mouse counts the game needs to move the view by one capture pixel, in percent. 100 is one count.</summary>
    [JsonPropertyName("counts_per_pixel")]
    public int CountsPerPixel { get; init; } = 100;

    /// <summary>Whether the aim point leads a moving target. Off aims at the box as it is.</summary>
    [JsonPropertyName("prediction")]
    public bool Prediction { get; init; } = true;

    /// <summary>Time in milliseconds for the velocity estimate to reach 63% of a step change. 0 means no smoothing.</summary>
    [JsonPropertyName("velocity_smoothing_ms")]
    public int VelocitySmoothingMs { get; init; } = 12;

    /// <summary>Clamps every value into its range.</summary>
    public AimSettings Clamped() => this with
    {
        SensitivityX = Math.Clamp(SensitivityX, MinSensitivity, MaxSensitivity),
        SensitivityY = Math.Clamp(SensitivityY, MinSensitivity, MaxSensitivity),
        TargetYOffset = Math.Clamp(TargetYOffset, 0, MaxTargetYOffset),
        MaxDelta = Math.Clamp(MaxDelta, 0, MaxDeltaLimit),
        CountsPerPixel = Math.Clamp(CountsPerPixel, MinSensitivity, MaxSensitivity),
        VelocitySmoothingMs = Math.Clamp(VelocitySmoothingMs, 0, MaxVelocitySmoothingMs),
    };
}
