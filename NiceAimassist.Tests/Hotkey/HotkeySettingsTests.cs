using NiceAimassist.Settings;

namespace NiceAimassist.Tests.Hotkey;

public sealed class HotkeySettingsTests
{
    [Fact]
    public void Blank_hotkeys_fall_back_to_the_default()
    {
        Assert.Equal(["F8"], AppSettings.Parse(@"{ ""hotkey"": { ""keys"": [""  ""] } }").Hotkey.Keys);
    }

    [Fact]
    public void Hotkeys_are_trimmed_and_deduplicated_ignoring_case()
    {
        Assert.Equal(["F9", "MouseX1"], AppSettings.Parse(@"{ ""hotkey"": { ""keys"": ["" F9 "", ""f9"", ""MouseX1"", """"] } }").Hotkey.Keys);
    }
}
