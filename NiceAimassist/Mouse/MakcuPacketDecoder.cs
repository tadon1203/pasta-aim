using System.Buffers.Binary;

namespace NiceAimassist.Mouse;

internal readonly record struct MakcuPacket(byte Command, byte[] Payload);

internal sealed class MakcuPacketDecoder
{
    private static readonly byte[] Header = [0xDE, 0xAD];

    private byte[] _pending = [];

    public void Feed(ReadOnlySpan<byte> bytes, List<MakcuPacket> packets)
    {
        byte[] data = _pending.Length == 0 ? bytes.ToArray() : [.. _pending, .. bytes];
        int start = 0;
        while (true)
        {
            ReadOnlySpan<byte> rest = data.AsSpan(start);
            int header = rest.IndexOf(Header.AsSpan());
            if (header < 0)
            {
                // The next Feed call completes a lone first header byte at the end.
                start = data.Length - (rest.Length > 0 && rest[^1] == Header[0] ? 1 : 0);
                break;
            }

            start += header;
            rest = data.AsSpan(start);
            if (rest.Length < 4)
            {
                break;
            }

            int length = BinaryPrimitives.ReadUInt16LittleEndian(rest[2..]);
            if (length > MakcuProtocol.MaxPayload)
            {
                // Skip only the header. Garbage data cannot stall the stream.
                start += Header.Length;
                continue;
            }

            int total = 5 + length;
            if (rest.Length < total)
            {
                break;
            }

            packets.Add(new MakcuPacket(rest[4], rest[5..total].ToArray()));
            start += total;
        }

        _pending = data[start..];
    }
}
