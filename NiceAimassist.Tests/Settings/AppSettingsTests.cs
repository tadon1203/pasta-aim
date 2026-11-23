using NiceAimassist.Capture;
using NiceAimassist.Detect;
using NiceAimassist.Hotkey;
using NiceAimassist.Mouse;
using NiceAimassist.Settings;

namespace NiceAimassist.Tests.Settings;

public sealed class AppSettingsTests
{
    [Fact]
    public void A_missing_section_takes_its_defaults_and_an_unknown_field_is_ignored()
    {
        AppSettings settings = AppSettings.Parse(@"{ ""version"": 2 }");

        Assert.Equal(["F8"], settings.Hotkey.Keys);
        Assert.Equal(0, settings.Display.Adapter);
        Assert.Equal(0, settings.Display.Output);
    }

    [Fact]
    public void Serialized_settings_use_nested_snake_case_names_and_read_back_equal()
    {
        AppSettings settings = new()
        {
            Detect = new DetectSettings { Model = "yolo.onnx" },
            Display = new DisplaySettings { Adapter = 1L << 32 | 7, Output = 2 },
            Mouse = new MouseSettings { Interpolation = Interpolation.Percent(30) },
            Hotkey = new HotkeySettings { Keys = ["F9", "xinput:a"] },
        };

        string json = settings.ToJson();

        Assert.Contains("\"target_y_offset\"", json);
        Assert.Contains("\"interpolation\": 30", json);
        Assert.Equal(settings, AppSettings.Parse(json));
    }

    [Fact]
    public void Text_that_is_not_an_object_is_unparsable()
    {
        Assert.Throws<System.Text.Json.JsonException>(() => AppSettings.Parse("[1, 2]"));
        Assert.Throws<System.Text.Json.JsonException>(() => AppSettings.Parse("null"));
        Assert.Throws<System.Text.Json.JsonException>(() => AppSettings.Parse(@"{ ""aim"": { ""sensitivity_x"": ""fast"" } }"));
    }
}
