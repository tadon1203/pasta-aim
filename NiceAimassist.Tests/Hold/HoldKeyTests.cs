using NiceAimassist.Hold;

namespace NiceAimassist.Tests.Hold;

public class HoldKeyTests
{
    [Theory]
    [InlineData("F8", 0x77)]
    [InlineData("KeyA", 0x41)]
    [InlineData("A", 0x41)]
    [InlineData("1", 0x31)]
    [InlineData("Digit1", 0x31)]
    [InlineData("ArrowUp", 0x26)]
    [InlineData("MouseX1", 0x05)]
    public void A_known_name_parses_to_its_virtual_key(string name, int code)
    {
        Assert.True(HoldKey.TryParse(name, out var key));
        Assert.Equal(code, key.VirtualKey);
    }

    [Theory]
    [InlineData("f8", "F8")]
    [InlineData("keya", "KeyA")]
    [InlineData("a", "A")]
    [InlineData("mousex1", "MouseX1")]
    public void A_name_parses_to_the_same_key_regardless_of_case(string name, string canonical)
    {
        Assert.True(HoldKey.TryParse(name, out var key));
        Assert.True(HoldKey.TryParse(canonical, out var expected));
        Assert.Equal(expected.VirtualKey, key.VirtualKey);
    }

    [Theory]
    [InlineData("xinput:a")]
    [InlineData("XInput:DPad_Up")]
    [InlineData("xinput:lt")]
    public void A_controller_name_parses_to_a_controller_input(string name)
    {
        Assert.True(HoldKey.TryParse(name, out var key));
        Assert.Equal(0, key.VirtualKey);
    }

    [Theory]
    [InlineData("")]
    [InlineData("xinput:left_stick_x")]
    [InlineData("F25")]
    [InlineData("Foo")]
    [InlineData("ab")]
    [InlineData("Shift")]
    public void An_unknown_name_is_rejected(string name)
    {
        Assert.False(HoldKey.TryParse(name, out _));
    }
}
