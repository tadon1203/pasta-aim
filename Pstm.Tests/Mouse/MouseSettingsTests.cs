using System.Text.Json;
using Pstm.Settings;
using Pstm.Mouse;

namespace Pstm.Tests.Mouse;

public sealed class MouseSettingsTests
{
    [Theory]
    [InlineData(@"{ ""mouse"": { ""smoothing"": ""auto"" } }", -1)]
    [InlineData(@"{ ""mouse"": { ""smoothing"": ""AUTO"" } }", -1)]
    [InlineData(@"{ ""mouse"": { ""smoothing"": 40 } }", 40)]
    [InlineData(@"{ ""mouse"": { ""smoothing"": 39.6 } }", 40)]
    [InlineData(@"{ ""mouse"": { ""smoothing"": 150 } }", 100)]
    [InlineData(@"{ ""mouse"": { ""smoothing"": -3 } }", 0)]
    public void Smoothing_reads_auto_or_a_clamped_percent(string json, int expectedWire)
    {
        Smoothing smoothing = AppSettings.Parse(json).Mouse.Smoothing;

        Assert.Equal(expectedWire == -1, smoothing.IsAuto);
        Assert.Equal(expectedWire == -1 ? null : expectedWire, smoothing.FixedPercent);
    }

    [Fact]
    public void Smoothing_of_another_kind_makes_the_file_unparsable()
    {
        Assert.Throws<JsonException>(() => AppSettings.Parse(@"{ ""mouse"": { ""smoothing"": ""fast"" } }"));
    }

    [Fact]
    public void Auto_serializes_as_the_string_auto()
    {
        string json = new AppSettings().ToJson();

        using JsonDocument document = JsonDocument.Parse(json);
        JsonElement smoothing = document.RootElement.GetProperty("mouse").GetProperty("smoothing");
        Assert.Equal(JsonValueKind.String, smoothing.ValueKind);
        Assert.Equal("auto", smoothing.GetString());
    }
}
