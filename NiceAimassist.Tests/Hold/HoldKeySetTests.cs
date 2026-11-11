using NiceAimassist.Hold;

namespace NiceAimassist.Tests.Hold;

public sealed class HoldKeySetTests
{
    private static HoldKey Key(string name)
    {
        Assert.True(HoldKey.TryParse(name, out HoldKey key));
        return key;
    }

    [Fact]
    public void A_name_that_is_not_a_known_key_is_rejected_at_startup()
    {
        Assert.Throws<InvalidOperationException>(() => new HoldKeySet(["NoSuchKey"]));
    }

    [Fact]
    public void Unknown_names_are_ignored_on_update()
    {
        var set = new HoldKeySet(["F8"]);

        set.Update(["NoSuchKey", "F9"]);

        Assert.Equal([Key("F9")], set.ActiveKeys);
    }

    [Fact]
    public void An_update_with_only_unknown_names_keeps_the_previous_keys()
    {
        var set = new HoldKeySet(["F9"]);

        set.Update(["NoSuchKey"]);

        Assert.Equal([Key("F9")], set.ActiveKeys);
    }
}
