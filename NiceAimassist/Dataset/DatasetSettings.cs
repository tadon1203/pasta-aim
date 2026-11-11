using System.Text.Json.Serialization;

namespace NiceAimassist.Dataset;

public sealed record DatasetSettings
{
    public const int MinSize = 64;
    public const int MaxSize = 4096;
    public const int MinRate = 1;
    public const int MaxRate = 2;
    public const int DefaultSize = 640;

    /// <summary>Whether recording runs while a hotkey is down.</summary>
    [JsonPropertyName("enabled")]
    public bool Enabled { get; init; } = true;

    /// <summary>Side of the centered square saved as the training image, in pixels.</summary>
    [JsonPropertyName("size")]
    public int Size { get; init; } = DefaultSize;

    /// <summary>Training images saved per second while recording.</summary>
    [JsonPropertyName("rate")]
    public int Rate { get; init; } = 2;

    public DatasetSettings Clamped() => this with
    {
        Size = Math.Clamp(Size, MinSize, MaxSize),
        Rate = Math.Clamp(Rate, MinRate, MaxRate),
    };
}
