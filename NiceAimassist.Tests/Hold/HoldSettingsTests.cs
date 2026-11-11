using NiceAimassist.Config;

namespace NiceAimassist.Tests.Hold;

public sealed class HoldSettingsTests
{
    [Fact]
    public void A_missing_hold_section_takes_the_default_key()
    {
        Assert.Equal(["F8"], Settings.Parse("{}").Hold.Keys);
    }

    [Fact]
    public void Blank_hold_keys_fall_back_to_the_default()
    {
        Assert.Equal(["F8"], Settings.Parse(@"{ ""hold"": { ""keys"": [""  ""] } }").Hold.Keys);
    }

    [Fact]
    public void Hold_keys_are_trimmed_and_deduplicated_ignoring_case()
    {
        Assert.Equal(["F9", "MouseX1"], Settings.Parse(@"{ ""hold"": { ""keys"": ["" F9 "", ""f9"", ""MouseX1"", """"] } }").Hold.Keys);
    }
}
