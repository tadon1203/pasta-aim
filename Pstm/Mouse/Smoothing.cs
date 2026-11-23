using System.Text.Json;
using System.Text.Json.Serialization;

namespace Pstm.Mouse;

/// <summary>How device smooths a move. The value is the device's Auto mode or a fixed percent in 0..100. The default value is Auto.</summary>
[JsonConverter(typeof(SmoothingJsonConverter))]
public readonly record struct Smoothing
{
    private readonly byte? _percent;

    private Smoothing(byte percent) => _percent = percent;

    public static Smoothing Auto => default;

    public static Smoothing Percent(int value) => new((byte)Math.Clamp(value, 0, 100));

    public bool IsAuto => _percent is null;

    /// <summary>The fixed percent, or null for Auto.</summary>
    public int? FixedPercent => _percent;
}

/// <summary>Reads and writes <see cref="Smoothing"/> as "auto" or a percent number.</summary>
public sealed class SmoothingJsonConverter : JsonConverter<Smoothing>
{
    public override Smoothing Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        switch (reader.TokenType)
        {
            case JsonTokenType.String when string.Equals(reader.GetString(), "auto", StringComparison.OrdinalIgnoreCase):
            case JsonTokenType.Null:
                return Smoothing.Auto;
            case JsonTokenType.Number:
                return Smoothing.Percent((int)Math.Round(Math.Clamp(reader.GetDouble(), 0, 100)));
            default:
                throw new JsonException("smoothing must be \"auto\" or a percent number");
        }
    }

    public override void Write(Utf8JsonWriter writer, Smoothing value, JsonSerializerOptions options)
    {
        if (value.FixedPercent is { } percent)
        {
            writer.WriteNumberValue(percent);
        }
        else
        {
            writer.WriteStringValue("auto");
        }
    }
}
