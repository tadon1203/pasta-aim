using NiceAimassist.Settings;

namespace NiceAimassist.Tests.Dataset;

public sealed class DatasetSettingsTests
{
    [Fact]
    public void Dataset_is_enabled_by_default_and_can_be_turned_off()
    {
        Assert.True(AppSettings.Parse("{}").Dataset.Enabled);
        Assert.False(AppSettings.Parse(@"{ ""dataset"": { ""enabled"": false } }").Dataset.Enabled);
    }
}
