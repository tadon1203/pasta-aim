using Pstm.Mouse;
using Pstm.Mouse.Serial;

namespace Pstm.Tests.Mouse.Serial;

public class SerialDeviceTests
{
    private static SerialDevice Connect(FakeDevice device) =>
        SerialDevice.Connect(["COM3"], (_, baud) => device.Open(baud));

    private static byte[] Last(FakeDevice device) => device.Frames[^1];

    [Fact]
    public void ADeviceAlreadyAt4000000BaudConnectsWithoutASwitch()
    {
        var device = new FakeDevice { Baud = 4_000_000 };

        using SerialDevice link = Connect(device);

        Assert.Equal(4_000_000, link.BaudRate);
        Assert.DoesNotContain(device.Frames, frame => frame[4] == DeviceProtocol.CommandSetBaud);
    }

    [Fact]
    public void ADeviceAt115200BaudIsSwitchedTo4000000AndTheLinkUsesTheNewRate()
    {
        var device = new FakeDevice { Baud = 115_200 };

        using SerialDevice link = Connect(device);

        Assert.Equal(4_000_000, device.Baud);
        Assert.Equal(4_000_000, link.BaudRate);
        Assert.Equal("COM3", link.PortName);
        Assert.Contains(device.Frames, frame => frame[4] == DeviceProtocol.CommandSetBaud && frame[5] == 0x00 && frame[6] == 0x09);
    }

    [Fact]
    public void ADeviceThatRefusesTheSwitchFailsTheConnectWithTheReason()
    {
        var device = new FakeDevice { Baud = 115_200 };
        device.AcceptedBauds.Remove(4_000_000);

        var error = Assert.Throws<InvalidOperationException>(() => Connect(device));

        Assert.Contains("after the baud switch", error.Message);
    }

    [Fact]
    public void ADeviceAnsweringAtNeitherRateFailsTheConnect()
    {
        var device = new FakeDevice { Baud = 9_600 };

        var error = Assert.Throws<InvalidOperationException>(() => Connect(device));

        Assert.Contains("did not answer at either baud rate", error.Message);
    }

    [Fact]
    public void ADeviceWithoutAMouseRouteIsRejectedWithAClearMessage()
    {
        var device = new FakeDevice { Baud = 4_000_000, Kinds = 0x02 };

        var error = Assert.Throws<InvalidOperationException>(() => Connect(device));

        Assert.Contains("no mouse route", error.Message);
        Assert.Contains("COM3", error.Message);
    }

    [Fact]
    public void AMoveIsOneWriteOfTheMoveRequest()
    {
        var device = new FakeDevice { Baud = 4_000_000 };
        using SerialDevice link = Connect(device);
        int framesBefore = device.Frames.Count;

        link.Move(10, -5);

        Assert.Equal(framesBefore + 1, device.Frames.Count);
        Assert.Equal<byte>([0xDE, 0xAD, 4, 0, 0x18, 0x0A, 0, 0xFB, 0xFF], Last(device));
    }

    [Fact]
    public void APercentSmoothingIsSentAsItsValue()
    {
        var device = new FakeDevice { Baud = 4_000_000 };
        using SerialDevice link = Connect(device);

        link.SetSmoothing(Smoothing.Percent(40));

        Assert.Equal<byte>([0xDE, 0xAD, 1, 0, 0x1F, 0x28], Last(device));
    }

    [Fact]
    public void AutoSmoothingIsSentAs255()
    {
        var device = new FakeDevice { Baud = 4_000_000 };
        using SerialDevice link = Connect(device);

        link.SetSmoothing(Smoothing.Auto);

        Assert.Equal<byte>([0xDE, 0xAD, 1, 0, 0x1F, 0xFF], Last(device));
    }

    [Fact]
    public void AnUnchangedSmoothingIsSentOnlyOnce()
    {
        var device = new FakeDevice { Baud = 4_000_000 };
        using SerialDevice link = Connect(device);
        int framesBefore = device.Frames.Count;

        link.SetSmoothing(Smoothing.Percent(40));
        link.SetSmoothing(Smoothing.Percent(40));
        Assert.Equal(framesBefore + 1, device.Frames.Count);

        link.SetSmoothing(Smoothing.Auto);
        Assert.Equal(framesBefore + 2, device.Frames.Count);
    }
}
