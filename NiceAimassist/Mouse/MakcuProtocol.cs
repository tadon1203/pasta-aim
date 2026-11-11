using System.Buffers.Binary;

namespace NiceAimassist.Mouse;

internal static class MakcuProtocol
{
    public const int MaxRequestLength = 9;
    public const int MaxPayload = 64;

    public const byte CommandDevice = 0x02;
    public const byte CommandMove = 0x18;
    public const byte CommandInterpolate = 0x1F;
    public const byte CommandSetBaud = 0xA5;

    public const byte KindMouse = 0x01;
    public const byte Reject = 0xFF;
    public const byte InterpolateAuto = 0xFF;

    private const byte Header0 = 0xDE;
    private const byte Header1 = 0xAD;

    public static int EncodeDevice(Span<byte> destination)
    {
        WriteHeader(destination, 0, CommandDevice);
        return 5;
    }

    public static int EncodeMove(Span<byte> destination, short x, short y)
    {
        WriteHeader(destination, 4, CommandMove);
        BinaryPrimitives.WriteInt16LittleEndian(destination[5..], x);
        BinaryPrimitives.WriteInt16LittleEndian(destination[7..], y);
        return 9;
    }

    public static int EncodeInterpolate(Span<byte> destination, byte value)
    {
        WriteHeader(destination, 1, CommandInterpolate);
        destination[5] = value;
        return 6;
    }

    /// <summary>The length byte of a baud switch counts the command byte. Other requests do not.</summary>
    public static int EncodeSetBaud(Span<byte> destination, uint baud)
    {
        WriteHeader(destination, 5, CommandSetBaud);
        BinaryPrimitives.WriteUInt32LittleEndian(destination[5..], baud);
        return 9;
    }

    public static bool IsReject(MakcuPacket packet) => packet.Payload is [Reject];

    /// <remarks>Any reply confirms Auto (255). A reject of Auto has the same bytes as its echo.</remarks>
    public static bool ConfirmsInterpolation(byte value, MakcuPacket packet) =>
        value == InterpolateAuto || (packet.Payload is [var echo] && echo == value);

    private static void WriteHeader(Span<byte> destination, ushort length, byte command)
    {
        destination[0] = Header0;
        destination[1] = Header1;
        BinaryPrimitives.WriteUInt16LittleEndian(destination[2..], length);
        destination[4] = command;
    }
}
