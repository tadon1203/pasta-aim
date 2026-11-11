using System.Text.Json;
using System.Text.Json.Serialization;
using NiceAimassist.Aim;
using NiceAimassist.Detect;
using NiceAimassist.Display;
using NiceAimassist.Hold;
using NiceAimassist.Mouse;

namespace NiceAimassist.Config;

/// <summary>
/// The settings file. A missing field takes its default. An unknown field is ignored.
/// An out-of-range value is clamped by <see cref="WithClampedRanges"/>.
/// </summary>
public sealed record Settings
{
    public const int MinCollectSize = 64;
    public const int MaxCollectSize = 4096;
    public const int MaxCollectRate = 2;
    public const int MinCollectRate = 1;

    private static readonly JsonSerializerOptions WriteOptions = new() { WriteIndented = true };

    [JsonPropertyName("detect")]
    public DetectSettings Detect { get; init; } = new();

    [JsonPropertyName("display")]
    public DisplaySettings Display { get; init; } = new();

    [JsonPropertyName("aim")]
    public AimSettings Aim { get; init; } = new();

    [JsonPropertyName("mouse")]
    public MouseSettings Mouse { get; init; } = new();

    [JsonPropertyName("hold")]
    public HoldSettings Hold { get; init; } = new();

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
        Hold = Hold.Clamped(),
        CollectSize = Math.Clamp(CollectSize, MinCollectSize, MaxCollectSize),
        CollectRate = Math.Clamp(CollectRate, MinCollectRate, MaxCollectRate),
        LogLevel = Enum.IsDefined(LogLevel) ? LogLevel : LogLevel.Info,
        Display = Display.Clamped(),
        Detect = Detect.Clamped(),
        Aim = Aim.Clamped(),
        Mouse = Mouse.Clamped(),
    };

    // The list property makes the generated comparison use references.
    public bool Equals(Settings? other) => other is not null && ToJson() == other.ToJson();

    public override int GetHashCode() => ToJson().GetHashCode();

    public string ToJson() => JsonSerializer.Serialize(this, WriteOptions);
}
