using NiceAimassist.Mouse;

namespace NiceAimassist.Tests.Mouse;

public class MakcuDeviceTests
{
    private static MakcuDevice Connect(FakeMakcu device) =>
        MakcuDevice.Connect(["COM3"], (_, baud) => device.Open(baud));

    private static byte[] Last(FakeMakcu device) => device.Frames[^1];

    [Fact]
    public void ADeviceAlreadyAt4000000BaudConnectsWithoutASwitch()
    {
        var device = new FakeMakcu { Baud = 4_000_000 };

        using MakcuDevice link = Connect(device);

        Assert.Equal(4_000_000, link.BaudRate);
        Assert.DoesNotContain(device.Frames, frame => frame[4] == MakcuProtocol.CommandSetBaud);
    }

    [Fact]
    public void ADeviceAt115200BaudIsSwitchedTo4000000AndTheLinkUsesTheNewRate()
    {
        var device = new FakeMakcu { Baud = 115_200 };

        using MakcuDevice link = Connect(device);

        Assert.Equal(4_000_000, device.Baud);
        Assert.Equal(4_000_000, link.BaudRate);
        Assert.Equal("COM3", link.PortName);
        Assert.Contains(device.Frames, frame => frame[4] == MakcuProtocol.CommandSetBaud && frame[5] == 0x00 && frame[6] == 0x09);
    }

    [Fact]
    public void ADeviceThatRefusesTheSwitchFailsTheConnectWithTheReason()
    {
        var device = new FakeMakcu { Baud = 115_200 };
        device.AcceptedBauds.Remove(4_000_000);

        var error = Assert.Throws<InvalidOperationException>(() => Connect(device));

        Assert.Contains("after the baud switch", error.Message);
    }

    [Fact]
    public void ADeviceAnsweringAtNeitherRateFailsTheConnect()
    {
        var device = new FakeMakcu { Baud = 9_600 };

        var error = Assert.Throws<InvalidOperationException>(() => Connect(device));

        Assert.Contains("did not answer at either baud rate", error.Message);
    }

    [Fact]
    public void ADeviceWithoutAMouseRouteIsRejectedWithAClearMessage()
    {
        var device = new FakeMakcu { Baud = 4_000_000, Kinds = 0x02 };

        var error = Assert.Throws<InvalidOperationException>(() => Connect(device));

        Assert.Contains("no mouse route", error.Message);
        Assert.Contains("COM3", error.Message);
    }

    [Fact]
    public void AMoveIsOneWriteOfTheMoveRequest()
    {
        var device = new FakeMakcu { Baud = 4_000_000 };
        using MakcuDevice link = Connect(device);
        int framesBefore = device.Frames.Count;

        link.Move(10, -5);

        Assert.Equal(framesBefore + 1, device.Frames.Count);
        Assert.Equal<byte>([0xDE, 0xAD, 4, 0, 0x18, 0x0A, 0, 0xFB, 0xFF], Last(device));
    }

    [Fact]
    public void APercentInterpolationIsSentAsItsValue()
    {
        var device = new FakeMakcu { Baud = 4_000_000 };
        using MakcuDevice link = Connect(device);

        link.SetInterpolation(Interpolation.Percent(40));

        Assert.Equal<byte>([0xDE, 0xAD, 1, 0, 0x1F, 0x28], Last(device));
    }

    [Fact]
    public void AutoInterpolationIsSentAs255()
    {
        var device = new FakeMakcu { Baud = 4_000_000 };
        using MakcuDevice link = Connect(device);

        link.SetInterpolation(Interpolation.Auto);

        Assert.Equal<byte>([0xDE, 0xAD, 1, 0, 0x1F, 0xFF], Last(device));
    }

    [Fact]
    public void APercentIsClampedToTheRangeWhenCreated()
    {
        Assert.Equal(Interpolation.Percent(100), Interpolation.Percent(150));
        Assert.Equal(Interpolation.Percent(0), Interpolation.Percent(-5));
    }
}
