using System.Text.Json;
using System.Text.Json.Serialization;

namespace NiceAimassist.Config;

/// <summary>
/// The settings file. Missing fields take their defaults, unknown fields are ignored,
/// and out-of-range values are clamped by <see cref="Validated"/>.
/// </summary>
public sealed record Settings
{
    public const string DefaultHoldKey = "F8";

    public const string DefaultCollectKey = "F9";
    public const int MinCollectSize = 64;
    public const int MaxCollectSize = 4096;
    public const int MaxCollectRate = 2;

    private static readonly string[] DefaultHoldKeys = [DefaultHoldKey];
    private static readonly string[] DefaultCollectKeys = [DefaultCollectKey];

    private static readonly JsonSerializerOptions WriteOptions = new() { WriteIndented = true };

    /// <summary>The .onnx file name inside models/ next to the executable. Null when none is chosen.</summary>
    [JsonPropertyName("model")]
    public string? Model { get; init; }

    /// <summary>The DirectX adapter LUID used by capture.</summary>
    [JsonPropertyName("display_adapter")]
    public long DisplayAdapter { get; init; }

    [JsonPropertyName("display_output")]
    public int DisplayOutput { get; init; }

    [JsonPropertyName("sensitivity_x")]
    public int SensitivityX { get; init; } = 100;

    [JsonPropertyName("sensitivity_y")]
    public int SensitivityY { get; init; } = 100;

    /// <summary>Minimum detection confidence in percent.</summary>
    [JsonPropertyName("confidence")]
    public int Confidence { get; init; } = 50;

    /// <summary>Aim point inside the target box in percent of its height. 0 is the top edge.</summary>
    [JsonPropertyName("target_y_offset")]
    public int TargetYOffset { get; init; } = 50;

    /// <summary>Per-frame cap on the movement length. 0 is no cap.</summary>
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

    /// <summary>Whether the collect key works. Turning it off also stops a running collection.</summary>
    [JsonPropertyName("collect_enabled")]
    public bool CollectEnabled { get; init; } = true;

    /// <summary>Side of the centered square saved as the training image, in pixels.</summary>
    [JsonPropertyName("collect_size")]
    public int CollectSize { get; init; } = 640;

    /// <summary>Training images saved per second while collecting.</summary>
    [JsonPropertyName("collect_rate")]
    public int CollectRate { get; init; } = 2;

    /// <summary>The key names that toggle training-data collection on a press.</summary>
    [JsonPropertyName("collect_keys")]
    public IReadOnlyList<string> CollectKeys { get; init; } = DefaultCollectKeys;

    /// <summary>The most detailed level written to the log file: error, warn, info, debug or trace.</summary>
    [JsonPropertyName("log_level")]
    [JsonConverter(typeof(JsonStringEnumConverter<LogLevel>))]
    public LogLevel LogLevel { get; init; } = LogLevel.Info;

    /// <summary>Parses a settings file and validates it. Throws <see cref="JsonException"/> when the text is not a settings object.</summary>
    public static Settings Parse(string json)
    {
        Settings settings = JsonSerializer.Deserialize<Settings>(json)
            ?? throw new JsonException("the settings file is empty or null");
        return settings.Validated();
    }

    /// <summary>Clamps every ranged value into its range and replaces an empty key or model name.</summary>
    public Settings Validated() => this with
    {
        SensitivityX = Math.Clamp(SensitivityX, 1, 10_000),
        SensitivityY = Math.Clamp(SensitivityY, 1, 10_000),
        Confidence = Math.Clamp(Confidence, 0, 100),
        TargetYOffset = Math.Clamp(TargetYOffset, 0, 100),
        MaxDelta = Math.Clamp(MaxDelta, 0, 32_767),
        VelocitySmoothing = Math.Clamp(VelocitySmoothing, 0, 95),
        Model = string.IsNullOrWhiteSpace(Model) ? null : Model,
        HoldKeys = NormalizedKeys(HoldKeys, DefaultHoldKeys),
        CollectSize = Math.Clamp(CollectSize, MinCollectSize, MaxCollectSize),
        CollectRate = Math.Clamp(CollectRate, 1, MaxCollectRate),
        CollectKeys = NormalizedKeys(CollectKeys, DefaultCollectKeys),
        LogLevel = Enum.IsDefined(LogLevel) ? LogLevel : LogLevel.Info,
    };

    private static IReadOnlyList<string> NormalizedKeys(IReadOnlyList<string>? names, string[] fallback)
    {
        string[] kept = [.. (names ?? []).Where(name => !string.IsNullOrWhiteSpace(name))
            .Select(name => name.Trim())
            .Distinct(StringComparer.OrdinalIgnoreCase)];
        return kept.Length == 0 ? fallback : kept;
    }

    // The list property would make the generated comparison use references.
    public bool Equals(Settings? other) => other is not null && ToJson() == other.ToJson();

    public override int GetHashCode() => ToJson().GetHashCode();

    public string ToJson() => JsonSerializer.Serialize(this, WriteOptions);
}
