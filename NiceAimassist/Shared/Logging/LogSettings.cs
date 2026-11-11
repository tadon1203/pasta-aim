using System.Text.Json.Serialization;

namespace NiceAimassist.Shared.Logging;

public sealed record LogSettings
{
    /// <summary>The most detailed level written to the log file: error, warn, info, debug or trace.</summary>
    [JsonPropertyName("level")]
    [JsonConverter(typeof(JsonStringEnumConverter<LogLevel>))]
    public LogLevel Level { get; init; } = LogLevel.Info;

    /// <summary>Clamps an unknown level number to info.</summary>
    public LogSettings Clamped() => this with { Level = Enum.IsDefined(Level) ? Level : LogLevel.Info };
}
