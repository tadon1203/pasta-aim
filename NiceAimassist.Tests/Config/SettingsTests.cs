using System.Text.Json;

using NiceAimassist.Config;
using NiceAimassist.Detect;
using NiceAimassist.Display;
using NiceAimassist.Hold;
using NiceAimassist.Mouse;

namespace NiceAimassist.Tests.Config;

public sealed class SettingsTests
{
    [Fact]
    public void A_missing_field_takes_its_default_and_an_unknown_field_is_ignored()
    {
        Settings settings = Settings.Parse(@"{ ""version"": 2 }");

        Assert.Equal(0, settings.Display.Adapter);
        Assert.Equal(0, settings.Display.Output);
    }

    [Fact]
    public void Serialized_settings_use_snake_case_names_and_read_back_equal()
    {
        Settings settings = new()
        {
            Detect = new DetectSettings { Model = "yolo.onnx" },
            Display = new DisplaySettings { Adapter = 1L << 32 | 7, Output = 2 },
            Mouse = new MouseSettings { Interpolation = Interpolation.Percent(30) },
            Hold = new HoldSettings { Keys = ["F9", "xinput:a"] },
        };

        string json = settings.ToJson();

        Assert.Contains("\"target_y_offset\"", json);
        Assert.Contains("\"interpolation\": 30", json);
        Assert.Equal(settings, Settings.Parse(json));
    }

    [Fact]
    public void Text_that_is_not_an_object_is_unparsable()
    {
        Assert.Throws<JsonException>(() => Settings.Parse("[1, 2]"));
        Assert.Throws<JsonException>(() => Settings.Parse("null"));
        Assert.Throws<JsonException>(() => Settings.Parse(@"{ ""aim"": { ""sensitivity_x"": ""fast"" } }"));
    }
}
