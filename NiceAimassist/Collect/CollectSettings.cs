using System.Text.Json.Serialization;

namespace NiceAimassist.Collect;

public sealed record CollectSettings
{
    public const int MinSize = 64;
    public const int MaxSize = 4096;
    public const int MinRate = 1;
    public const int MaxRate = 2;
    public const int DefaultSize = 640;

    /// <summary>Whether collection runs while the hold key is down.</summary>
    [JsonPropertyName("enabled")]
    public bool Enabled { get; init; } = true;

    /// <summary>Side of the centered square saved as the training image, in pixels.</summary>
    [JsonPropertyName("size")]
    public int Size { get; init; } = DefaultSize;

    /// <summary>Training images saved per second while collecting.</summary>
    [JsonPropertyName("rate")]
    public int Rate { get; init; } = 2;

    public CollectSettings Clamped() => this with
    {
        Size = Math.Clamp(Size, MinSize, MaxSize),
        Rate = Math.Clamp(Rate, MinRate, MaxRate),
    };
}
