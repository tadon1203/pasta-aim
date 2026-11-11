using NiceAimassist.Mouse;
using NiceAimassist.Mouse.Makcu;

namespace NiceAimassist.Tests.Mouse.Makcu;

public class MakcuProtocolTests
{
    [Fact]
    public void DeviceRequestEncodesToTheDocumentedBytes()
    {
        var buffer = new byte[MakcuProtocol.MaxRequestLength];
        int length = MakcuProtocol.EncodeDevice(buffer);
        Assert.Equal<byte>([0xDE, 0xAD, 0, 0, 0x02], buffer[..length]);
    }

    [Fact]
    public void MoveEncodesSignedCoordinatesLittleEndian()
    {
        var buffer = new byte[MakcuProtocol.MaxRequestLength];
        int length = MakcuProtocol.EncodeMove(buffer, 10, -5);
        Assert.Equal<byte>([0xDE, 0xAD, 4, 0, 0x18, 0x0A, 0, 0xFB, 0xFF], buffer[..length]);
    }

    [Fact]
    public void AutoInterpolationIsSentAs255()
    {
        var buffer = new byte[MakcuProtocol.MaxRequestLength];
        int length = MakcuProtocol.EncodeInterpolate(buffer, 255);
        Assert.Equal<byte>([0xDE, 0xAD, 1, 0, 0x1F, 0xFF], buffer[..length]);
    }

    [Fact]
    public void PercentInterpolationIsSentAsItsValue()
    {
        var buffer = new byte[MakcuProtocol.MaxRequestLength];
        int length = MakcuProtocol.EncodeInterpolate(buffer, 40);
        Assert.Equal<byte>([0xDE, 0xAD, 1, 0, 0x1F, 0x28], buffer[..length]);
    }

    [Fact]
    public void BaudSwitchEncodesItsLengthCountingTheCommandByte()
    {
        var buffer = new byte[MakcuProtocol.MaxRequestLength];
        int length = MakcuProtocol.EncodeSetBaud(buffer, 4_000_000);
        Assert.Equal<byte>([0xDE, 0xAD, 5, 0, 0xA5, 0x00, 0x09, 0x3D, 0x00], buffer[..length]);
    }

    [Fact]
    public void APercentEchoConfirmsItsValueAndOtherRepliesDoNot()
    {
        Assert.True(MakcuProtocol.ConfirmsInterpolation(40, Reply(MakcuProtocol.CommandInterpolate, [40])));
        Assert.False(MakcuProtocol.ConfirmsInterpolation(40, Reply(MakcuProtocol.CommandInterpolate, [7])));
        Assert.False(MakcuProtocol.ConfirmsInterpolation(40, Reply(MakcuProtocol.CommandInterpolate, [MakcuProtocol.Reject])));
    }

    [Fact]
    public void AutoIsConfirmedByAnyReplyBecauseItsRejectHasTheSameBytesAsItsEcho()
    {
        Assert.True(MakcuProtocol.ConfirmsInterpolation(255, Reply(MakcuProtocol.CommandInterpolate, [MakcuProtocol.Reject])));
        Assert.True(MakcuProtocol.ConfirmsInterpolation(255, Reply(MakcuProtocol.CommandInterpolate, [7])));
    }

    [Fact]
    public void ARejectIsTheSingleFfPayload()
    {
        Assert.True(MakcuProtocol.IsReject(Reply(MakcuProtocol.CommandDevice, [MakcuProtocol.Reject])));
        Assert.False(MakcuProtocol.IsReject(Reply(MakcuProtocol.CommandDevice, [0x01])));
    }

    [Fact]
    public void DecoderReturnsPacketsJoinedInOneFeed()
    {
        byte[] bytes = [.. Frame(0x02, [0x01]), .. Frame(0x1F, [0xFF])];
        var packets = new List<MakcuPacket>();
        new MakcuPacketDecoder().Feed(bytes, packets);
        Assert.Equal("02:01|1F:FF", Describe(packets));
    }

    [Fact]
    public void DecoderReadsAPacketSplitAtEveryByte()
    {
        byte[] bytes = Frame(0x04, [0xEE, 0x0F, 0, 0]);
        var decoder = new MakcuPacketDecoder();
        var packets = new List<MakcuPacket>();
        foreach (byte value in bytes)
        {
            decoder.Feed([value], packets);
        }

        Assert.Equal("04:EE0F0000", Describe(packets));
    }

    [Fact]
    public void DecoderSkipsGarbageBeforeTheHeader()
    {
        byte[] bytes = [0x00, 0xDE, 0x13, 0xAD, .. Frame(0x02, [0x01])];
        var packets = new List<MakcuPacket>();
        new MakcuPacketDecoder().Feed(bytes, packets);
        Assert.Equal("02:01", Describe(packets));
    }

    [Fact]
    public void DecoderDropsAHeaderWhoseLengthIsImpossibleAndKeepsTheNextPacket()
    {
        byte[] bytes = [0xDE, 0xAD, 0xFF, 0xFF, 0x02, .. Frame(0x04, [1, 2, 3, 4])];
        var packets = new List<MakcuPacket>();
        new MakcuPacketDecoder().Feed(bytes, packets);
        Assert.Equal("04:01020304", Describe(packets));
    }

    private static string Describe(IEnumerable<MakcuPacket> packets) =>
        string.Join("|", packets.Select(packet => $"{packet.Command:X2}:{Convert.ToHexString(packet.Payload)}"));

    private static MakcuPacket Reply(byte command, byte[] payload) => new(command, payload);

    private static byte[] Frame(byte command, byte[] payload)
    {
        var frame = new List<byte> { 0xDE, 0xAD, (byte)payload.Length, 0, command };
        frame.AddRange(payload);
        return frame.ToArray();
    }
}
