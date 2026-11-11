using System.Text.Json;
using System.Text.Json.Serialization;
using NiceAimassist.Detect;
using NiceAimassist.Display;

namespace NiceAimassist.Config;

/// <summary>
/// The settings file. A missing field takes its default. An unknown field is ignored.
/// An out-of-range value is clamped by <see cref="WithClampedRanges"/>.
/// </summary>
public sealed record Settings
{
    public const string DefaultHoldKey = "F8";

    public const int MinCollectSize = 64;
    public const int MaxCollectSize = 4096;
    public const int MaxCollectRate = 2;
    public const int MinCollectRate = 1;

    public const int MinSensitivity = 1;
    public const int MaxSensitivity = 10_000;
    public const int MaxTargetYOffset = 100;
    public const int MaxDeltaLimit = 32_767;
    public const int MaxVelocitySmoothing = 95;

    private static readonly string[] DefaultHoldKeys = [DefaultHoldKey];

    private static readonly JsonSerializerOptions WriteOptions = new() { WriteIndented = true };

    [JsonPropertyName("detect")]
    public DetectSettings Detect { get; init; } = new();

    [JsonPropertyName("display")]
    public DisplaySettings Display { get; init; } = new();

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

    [JsonPropertyName("interpolation")]
    public Interpolation Interpolation { get; init; }

    /// <summary>
    /// The key names that hold the aim, such as "F8". The aim is held while any of them is down.
    /// </summary>
    [JsonPropertyName("hold_keys")]
    public IReadOnlyList<string> HoldKeys { get; init; } = DefaultHoldKeys;

    /// <summary>Whether collection runs while the hold key is down.</summary>
    [JsonPropertyName("collect_enabled")]
    public bool CollectEnabled { get; init; } = true;

    /// <summary>Side of the centered square saved as the training image, in pixels.</summary>
    [JsonPropertyName("collect_size")]
    public int CollectSize { get; init; } = 640;

    /// <summary>Training images saved per second while collecting.</summary>
    [JsonPropertyName("collect_rate")]
    public int CollectRate { get; init; } = 2;

    /// <summary>The most detailed level written to the log file: error, warn, info, debug or trace.</summary>
    [JsonPropertyName("log_level")]
    [JsonConverter(typeof(JsonStringEnumConverter<LogLevel>))]
    public LogLevel LogLevel { get; init; } = LogLevel.Info;

    /// <summary>Parses a settings file and validates it. Throws <see cref="JsonException"/> when the text is not a settings object.</summary>
    public static Settings Parse(string json)
    {
        Settings settings = JsonSerializer.Deserialize<Settings>(json)
            ?? throw new JsonException("the settings file is empty or null");
        return settings.WithClampedRanges();
    }

    /// <summary>Clamps every ranged value into its range. Replaces an empty key or model name.</summary>
    public Settings WithClampedRanges() => this with
    {
        SensitivityX = Math.Clamp(SensitivityX, MinSensitivity, MaxSensitivity),
        SensitivityY = Math.Clamp(SensitivityY, MinSensitivity, MaxSensitivity),
        TargetYOffset = Math.Clamp(TargetYOffset, 0, MaxTargetYOffset),
        MaxDelta = Math.Clamp(MaxDelta, 0, MaxDeltaLimit),
        VelocitySmoothing = Math.Clamp(VelocitySmoothing, 0, MaxVelocitySmoothing),
        HoldKeys = NormalizedKeys(HoldKeys),
        CollectSize = Math.Clamp(CollectSize, MinCollectSize, MaxCollectSize),
        CollectRate = Math.Clamp(CollectRate, MinCollectRate, MaxCollectRate),
        LogLevel = Enum.IsDefined(LogLevel) ? LogLevel : LogLevel.Info,
        Display = Display.Clamped(),
        Detect = Detect.Clamped(),
    };

    private static IReadOnlyList<string> NormalizedKeys(IReadOnlyList<string>? names)
    {
        string[] kept = [.. (names ?? []).Where(name => !string.IsNullOrWhiteSpace(name))
            .Select(name => name.Trim())
            .Distinct(StringComparer.OrdinalIgnoreCase)];
        return kept.Length == 0 ? DefaultHoldKeys : kept;
    }

    // The list property makes the generated comparison use references.
    public bool Equals(Settings? other) => other is not null && ToJson() == other.ToJson();

    public override int GetHashCode() => ToJson().GetHashCode();

    public string ToJson() => JsonSerializer.Serialize(this, WriteOptions);
}
