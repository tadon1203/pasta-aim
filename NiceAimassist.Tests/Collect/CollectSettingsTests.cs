using NiceAimassist.Collect;
using NiceAimassist.Config;

namespace NiceAimassist.Tests.Collect;

public sealed class CollectSettingsTests
{
    [Fact]
    public void Size_and_rate_default_and_clamp_into_their_range()
    {
        CollectSettings defaults = Settings.Parse("{}").Collect;
        CollectSettings clamped = Settings.Parse(@"{ ""collect"": { ""size"": 8, ""rate"": 9 } }").Collect;

        Assert.Equal((640, 2), (defaults.Size, defaults.Rate));
        Assert.Equal((CollectSettings.MinSize, CollectSettings.MaxRate), (clamped.Size, clamped.Rate));
        Assert.Equal(CollectSettings.MaxSize, Settings.Parse(@"{ ""collect"": { ""size"": 99999 } }").Collect.Size);
    }

    [Fact]
    public void Collect_is_enabled_by_default_and_can_be_turned_off()
    {
        Assert.True(Settings.Parse("{}").Collect.Enabled);
        Assert.False(Settings.Parse(@"{ ""collect"": { ""enabled"": false } }").Collect.Enabled);
    }
}
