using System.Text.Json;
using NiceAimassist.Config;
using NiceAimassist.Mouse;

namespace NiceAimassist.Tests.Mouse;

public sealed class MouseSettingsTests
{
    [Fact]
    public void A_missing_mouse_section_takes_auto_interpolation()
    {
        Assert.Equal(Interpolation.Auto, Settings.Parse("{}").Mouse.Interpolation);
    }

    [Theory]
    [InlineData(@"{ ""mouse"": { ""interpolation"": ""auto"" } }", -1)]
    [InlineData(@"{ ""mouse"": { ""interpolation"": ""AUTO"" } }", -1)]
    [InlineData(@"{ ""mouse"": { ""interpolation"": 40 } }", 40)]
    [InlineData(@"{ ""mouse"": { ""interpolation"": 39.6 } }", 40)]
    [InlineData(@"{ ""mouse"": { ""interpolation"": 150 } }", 100)]
    [InlineData(@"{ ""mouse"": { ""interpolation"": -3 } }", 0)]
    public void Interpolation_reads_auto_or_a_clamped_percent(string json, int expectedWire)
    {
        Interpolation interpolation = Settings.Parse(json).Mouse.Interpolation;

        Assert.Equal(expectedWire == -1, interpolation.IsAuto);
        Assert.Equal(expectedWire == -1 ? null : expectedWire, interpolation.FixedPercent);
    }

    [Fact]
    public void Interpolation_of_another_kind_makes_the_file_unparsable()
    {
        Assert.Throws<JsonException>(() => Settings.Parse(@"{ ""mouse"": { ""interpolation"": ""fast"" } }"));
    }

    [Fact]
    public void Auto_serializes_as_the_string_auto()
    {
        string json = new Settings().ToJson();

        using JsonDocument document = JsonDocument.Parse(json);
        JsonElement interpolation = document.RootElement.GetProperty("mouse").GetProperty("interpolation");
        Assert.Equal(JsonValueKind.String, interpolation.ValueKind);
        Assert.Equal("auto", interpolation.GetString());
    }
}
