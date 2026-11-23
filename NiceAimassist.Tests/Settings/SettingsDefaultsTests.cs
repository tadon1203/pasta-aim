using NiceAimassist.Settings;
using NiceAimassist.Shared.Logging;

namespace NiceAimassist.Tests.Settings;

public sealed class SettingsDefaultsTests
{
    [Fact]
    public void A_missing_section_takes_its_defaults()
    {
        AppSettings settings = AppSettings.Parse("{}");

        Assert.Equal(["F8"], settings.Hotkey.Keys);
        Assert.Equal(50, settings.Detect.Confidence);
        Assert.Null(settings.Detect.Model);
        Assert.Equal(
            (100, 100, 50, 512, 12, 100),
            (settings.Aim.SensitivityX, settings.Aim.SensitivityY, settings.Aim.TargetYOffset,
                settings.Aim.MaxDelta, settings.Aim.VelocitySmoothingMs, settings.Aim.CountsPerPixel));
        Assert.True(settings.Aim.Prediction);
        Assert.True(settings.Mouse.Interpolation.IsAuto);
        Assert.True(settings.Dataset.Enabled);
        Assert.Equal(LogLevel.Info, settings.Log.Level);
        Assert.Equal((0L, 0), (settings.Display.Adapter, settings.Display.Output));

        Assert.False(AppSettings.Parse(@"{ ""dataset"": { ""enabled"": false } }").Dataset.Enabled);
    }
}
