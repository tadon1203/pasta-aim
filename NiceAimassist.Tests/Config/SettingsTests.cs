using System.Text.Json;

using NiceAimassist.Config;
using NiceAimassist.Detect;
using NiceAimassist.Display;
using NiceAimassist.Mouse;

namespace NiceAimassist.Tests.Config;

public sealed class SettingsTests
{
    [Fact]
    public void A_missing_field_takes_its_default_and_an_unknown_field_is_ignored()
    {
        Settings settings = Settings.Parse(@"{ ""version"": 2 }");

        Assert.Equal(["F8"], settings.HoldKeys);
        Assert.Equal(0, settings.Display.Adapter);
        Assert.Equal(0, settings.Display.Output);
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
    public void Blank_hold_keys_fall_back_to_the_default()
    {
        Settings settings = Settings.Parse(@"{ ""hold_keys"": [""  ""] }");

        Assert.Equal(["F8"], settings.HoldKeys);
    }

    [Fact]
    public void Hold_keys_are_trimmed_and_deduplicated_ignoring_case()
    {
        Settings settings = Settings.Parse(@"{ ""hold_keys"": ["" F9 "", ""f9"", ""MouseX1"", """"] }");

        Assert.Equal(["F9", "MouseX1"], settings.HoldKeys);
    }

    [Fact]
    public void Serialized_settings_use_snake_case_names_and_read_back_equal()
    {
        Settings settings = new()
        {
            Detect = new DetectSettings { Model = "yolo.onnx" },
            Display = new DisplaySettings { Adapter = 1L << 32 | 7, Output = 2 },
            Mouse = new MouseSettings { Interpolation = Interpolation.Percent(30) },
            HoldKeys = ["F9", "xinput:a"],
        };

        string json = settings.ToJson();

        Assert.Contains("\"target_y_offset\"", json);
        Assert.Contains("\"interpolation\": 30", json);
        Assert.Equal(settings, Settings.Parse(json));
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
        Assert.Throws<JsonException>(() => Settings.Parse(@"{ ""aim"": { ""sensitivity_x"": ""fast"" } }"));
    }
}
