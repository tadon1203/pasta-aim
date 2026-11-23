using Pstm.Settings;
using Pstm.Shared.Logging;

namespace Pstm.Tests.Settings;

public sealed class SettingsDefaultsTests
{
    [Fact]
    public void A_missing_section_takes_its_defaults()
    {
        AppSettings settings = AppSettings.Parse("{}");

        Assert.Equal(["F8"], settings.Hotkey.Keys);
        Assert.Equal(50, settings.Detect.Threshold);
        Assert.Null(settings.Detect.Model);
        Assert.Equal(
            (100, 100, 50, 512, 12, 100),
            (settings.Correction.SensitivityX, settings.Correction.SensitivityY, settings.Correction.Offset,
                settings.Correction.MaxDelta, settings.Correction.LeadSmoothingMs, settings.Correction.LeadScale));
        Assert.True(settings.Correction.Lead);
        Assert.True(settings.Mouse.Smoothing.IsAuto);
        Assert.True(settings.Samples.Enabled);
        Assert.Equal(LogLevel.Info, settings.Log.Level);
        Assert.Equal((0L, 0), (settings.Display.Adapter, settings.Display.Output));

        Assert.False(AppSettings.Parse(@"{ ""samples"": { ""enabled"": false } }").Samples.Enabled);
    }
}
