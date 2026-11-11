using System.Text.Json;

using NiceAimassist.Config;
using NiceAimassist.Display;

namespace NiceAimassist.Tests.Config;

public sealed class SettingsTests
{
    [Fact]
    public void A_missing_field_takes_its_default_and_an_unknown_field_is_ignored()
    {
        Settings settings = Settings.Parse(@"{ ""sensitivity_x"": 250, ""version"": 2 }");

        Assert.Equal(250, settings.SensitivityX);
        Assert.Equal(100, settings.SensitivityY);
        Assert.Equal(50, settings.Confidence);
        Assert.Equal(50, settings.TargetYOffset);
        Assert.Equal(512, settings.MaxDelta);
        Assert.Equal(50, settings.VelocitySmoothing);
        Assert.Equal(Interpolation.Auto, settings.Interpolation);
        Assert.Equal(["F8"], settings.HoldKeys);
        Assert.Null(settings.Model);
        Assert.Equal(0, settings.Display.Adapter);
        Assert.Equal(0, settings.Display.Output);
    }

    [Fact]
    public void Out_of_range_values_are_clamped_into_their_range()
    {
        Settings settings = Settings.Parse(
            @"{ ""sensitivity_x"": 0, ""sensitivity_y"": 20000, ""confidence"": 150,
                ""target_y_offset"": -1, ""max_delta"": -5, ""velocity_smoothing"": 200 }");

        Assert.Equal(1, settings.SensitivityX);
        Assert.Equal(10_000, settings.SensitivityY);
        Assert.Equal(100, settings.Confidence);
        Assert.Equal(0, settings.TargetYOffset);
        Assert.Equal(0, settings.MaxDelta);
        Assert.Equal(95, settings.VelocitySmoothing);
    }

    [Fact]
    public void Collect_settings_default_and_clamp()
    {
        Settings defaults = Settings.Parse("{}");
        Settings clamped = Settings.Parse(@"{ ""collect_size"": 8, ""collect_rate"": 9 }");

        Assert.Equal((640, 2), (defaults.CollectSize, defaults.CollectRate));
        Assert.Equal((Settings.MinCollectSize, Settings.MaxCollectRate), (clamped.CollectSize, clamped.CollectRate));
        Assert.Equal(Settings.MaxCollectSize, Settings.Parse(@"{ ""collect_size"": 99999 }").CollectSize);
    }

    [Fact]
    public void Collect_is_enabled_by_default_and_can_be_turned_off()
    {
        Assert.True(Settings.Parse("{}").CollectEnabled);
        Assert.False(Settings.Parse(@"{ ""collect_enabled"": false }").CollectEnabled);
    }

    [Fact]
    public void Blank_hold_keys_and_an_empty_model_fall_back_to_the_defaults()
    {
        Settings settings = Settings.Parse(@"{ ""hold_keys"": [""  ""], ""model"": """" }");

        Assert.Equal(["F8"], settings.HoldKeys);
        Assert.Null(settings.Model);
    }

    [Fact]
    public void Hold_keys_are_trimmed_and_deduplicated_ignoring_case()
    {
        Settings settings = Settings.Parse(@"{ ""hold_keys"": ["" F9 "", ""f9"", ""MouseX1"", """"] }");

        Assert.Equal(["F9", "MouseX1"], settings.HoldKeys);
    }

    [Theory]
    [InlineData(@"{ ""interpolation"": ""auto"" }", -1)]
    [InlineData(@"{ ""interpolation"": ""AUTO"" }", -1)]
    [InlineData(@"{ ""interpolation"": 40 }", 40)]
    [InlineData(@"{ ""interpolation"": 39.6 }", 40)]
    [InlineData(@"{ ""interpolation"": 150 }", 100)]
    [InlineData(@"{ ""interpolation"": -3 }", 0)]
    public void Interpolation_reads_auto_or_a_clamped_percent(string json, int expectedWire)
    {
        Settings settings = Settings.Parse(json);

        Assert.Equal(expectedWire == -1, settings.Interpolation.IsAuto);
        Assert.Equal(expectedWire == -1 ? null : expectedWire, settings.Interpolation.FixedPercent);
    }

    [Fact]
    public void Interpolation_of_another_kind_makes_the_file_unparsable()
    {
        Assert.Throws<JsonException>(() => Settings.Parse(@"{ ""interpolation"": ""fast"" }"));
    }

    [Fact]
    public void Serialized_settings_use_snake_case_names_and_read_back_equal()
    {
        Settings settings = new()
        {
            Model = "yolo.onnx",
            Display = new DisplaySettings { Adapter = 1L << 32 | 7, Output = 2 },
            Interpolation = Interpolation.Percent(30),
            HoldKeys = ["F9", "xinput:a"],
        };

        string json = settings.ToJson();

        Assert.Contains("\"target_y_offset\"", json);
        Assert.Contains("\"interpolation\": 30", json);
        Assert.Equal(settings, Settings.Parse(json));
    }

    [Fact]
    public void Auto_serializes_as_the_string_auto()
    {
        string json = new Settings().ToJson();

        Assert.Contains("\"interpolation\": \"auto\"", json);
        using JsonDocument document = JsonDocument.Parse(json);
        Assert.Equal(JsonValueKind.String, document.RootElement.GetProperty("interpolation").ValueKind);
    }

    [Theory]
    [InlineData(@"{}", LogLevel.Info)]
    [InlineData(@"{ ""log_level"": ""trace"" }", LogLevel.Trace)]
    [InlineData(@"{ ""log_level"": ""Warn"" }", LogLevel.Warn)]
    [InlineData(@"{ ""log_level"": 9 }", LogLevel.Info)]
    public void Log_level_reads_a_name_and_falls_back_to_info_for_an_unknown_number(string json, LogLevel expected)
    {
        Assert.Equal(expected, Settings.Parse(json).LogLevel);
    }

    [Fact]
    public void Log_level_serializes_as_a_name()
    {
        Settings settings = new() { LogLevel = LogLevel.Debug };

        Assert.Contains("\"log_level\": \"Debug\"", settings.ToJson());
        Assert.Equal(settings, Settings.Parse(settings.ToJson()));
    }

    [Fact]
    public void Text_that_is_not_an_object_is_unparsable()
    {
        Assert.Throws<JsonException>(() => Settings.Parse("[1, 2]"));
        Assert.Throws<JsonException>(() => Settings.Parse("null"));
        Assert.Throws<JsonException>(() => Settings.Parse(@"{ ""sensitivity_x"": ""fast"" }"));
    }
}
