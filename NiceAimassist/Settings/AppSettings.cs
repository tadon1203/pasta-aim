using System.Text.Json;
using System.Text.Json.Serialization;
using NiceAimassist.Aim;
using NiceAimassist.Collect;
using NiceAimassist.Detect;
using NiceAimassist.Display;
using NiceAimassist.Hold;
using NiceAimassist.Logging;
using NiceAimassist.Mouse;

namespace NiceAimassist.Settings;

/// <summary>
/// The settings file. A missing section takes its defaults. An unknown field is ignored.
/// An out-of-range value is clamped by <see cref="Clamped"/>.
/// </summary>
public sealed record AppSettings
{
    private static readonly JsonSerializerOptions WriteOptions = new() { WriteIndented = true };

    [JsonPropertyName("display")]
    public DisplaySettings Display { get; init; } = new();

    [JsonPropertyName("detect")]
    public DetectSettings Detect { get; init; } = new();

    [JsonPropertyName("aim")]
    public AimSettings Aim { get; init; } = new();

    [JsonPropertyName("mouse")]
    public MouseSettings Mouse { get; init; } = new();

    [JsonPropertyName("hold")]
    public HoldSettings Hold { get; init; } = new();

    [JsonPropertyName("collect")]
    public CollectSettings Collect { get; init; } = new();

    [JsonPropertyName("log")]
    public LogSettings Log { get; init; } = new();

    /// <summary>Parses a settings file and validates it. Throws <see cref="JsonException"/> when the text is not a settings object.</summary>
    public static AppSettings Parse(string json)
    {
        AppSettings settings = JsonSerializer.Deserialize<AppSettings>(json)
            ?? throw new JsonException("the settings file is empty or null");
        return settings.Clamped();
    }

    /// <summary>Clamps every section into its range.</summary>
    public AppSettings Clamped() => this with
    {
        Display = Display.Clamped(),
        Detect = Detect.Clamped(),
        Aim = Aim.Clamped(),
        Mouse = Mouse.Clamped(),
        Hold = Hold.Clamped(),
        Collect = Collect.Clamped(),
        Log = Log.Clamped(),
    };

    // The list property makes the generated comparison use references.
    public bool Equals(AppSettings? other) => other is not null && ToJson() == other.ToJson();

    public override int GetHashCode() => ToJson().GetHashCode();

    public string ToJson() => JsonSerializer.Serialize(this, WriteOptions);
}
