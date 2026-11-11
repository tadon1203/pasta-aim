using NiceAimassist.Dataset;
using NiceAimassist.Settings;

namespace NiceAimassist.Tests.Dataset;

public sealed class DatasetSettingsTests
{
    [Fact]
    public void Size_and_rate_default_and_clamp_into_their_range()
    {
        DatasetSettings defaults = AppSettings.Parse("{}").Dataset;
        DatasetSettings clamped = AppSettings.Parse(@"{ ""dataset"": { ""size"": 8, ""rate"": 9 } }").Dataset;

        Assert.Equal((640, 2), (defaults.Size, defaults.Rate));
        Assert.Equal((DatasetSettings.MinSize, DatasetSettings.MaxRate), (clamped.Size, clamped.Rate));
        Assert.Equal(DatasetSettings.MaxSize, AppSettings.Parse(@"{ ""dataset"": { ""size"": 99999 } }").Dataset.Size);
    }

    [Fact]
    public void Dataset_is_enabled_by_default_and_can_be_turned_off()
    {
        Assert.True(AppSettings.Parse("{}").Dataset.Enabled);
        Assert.False(AppSettings.Parse(@"{ ""dataset"": { ""enabled"": false } }").Dataset.Enabled);
    }
}
