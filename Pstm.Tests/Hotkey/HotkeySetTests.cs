using Pstm.Hotkey;

namespace Pstm.Tests.Hotkey;

public sealed class HotkeySetTests
{
    private static Key ParseKey(string name)
    {
        Assert.True(Key.TryParse(name, out Key key));
        return key;
    }

    [Fact]
    public void A_set_with_only_unknown_names_has_no_keys()
    {
        var set = new HotkeySet(["NoSuchKey"]);

        Assert.False(set.HasKeys);
    }

    [Fact]
    public void Unknown_names_are_ignored_on_update()
    {
        var set = new HotkeySet(["F8"]);

        set.Update(["NoSuchKey", "F9"]);

        Assert.Equal([ParseKey("F9")], set.ActiveKeys);
    }

    [Fact]
    public void An_update_with_only_unknown_names_keeps_the_previous_keys()
    {
        var set = new HotkeySet(["F9"]);

        set.Update(["NoSuchKey"]);

        Assert.Equal([ParseKey("F9")], set.ActiveKeys);
    }
}
