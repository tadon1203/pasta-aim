using System.Buffers.Binary;
using NiceAimassist.Makcu;

namespace NiceAimassist.Tests.Makcu;

/// <summary>A simulated MAKCU. It answers only on a port whose baud rate matches its own. The device does the same.</summary>
internal sealed class FakeMakcu
{
    public int Baud { get; set; } = 115_200;

    public HashSet<int> AcceptedBauds { get; } = [115_200, 4_000_000];

    public byte Kinds { get; set; } = 0x01;

    public List<byte[]> Frames { get; } = [];

    public FakeMakcuPort Open(int baud) => new(this, baud);

    internal byte[] Answer(ReadOnlySpan<byte> frame)
    {
        byte command = frame[4];
        switch (command)
        {
            case Protocol.CommandDevice:
                return Reply(command, [Kinds]);
            case Protocol.CommandInterpolate:
                return Reply(command, [frame[5]]);
            case Protocol.CommandSetBaud:
                int baud = (int)BinaryPrimitives.ReadUInt32LittleEndian(frame[5..9]);
                if (AcceptedBauds.Contains(baud))
                {
                    Baud = baud;
                    return [];
                }

                return Reply(command, [Protocol.Reject]);
            default:
                return [];
        }
    }

    private static byte[] Reply(byte command, byte[] payload)
    {
        var reply = new List<byte> { 0xDE, 0xAD, (byte)payload.Length, 0, command };
        reply.AddRange(payload);
        return reply.ToArray();
    }
}

internal sealed class FakeMakcuPort : IMakcuPort
{
    private readonly FakeMakcu _device;
    private readonly Queue<byte> _replies = new();
    private int _baud;

    internal FakeMakcuPort(FakeMakcu device, int baud)
    {
        _device = device;
        _baud = baud;
    }

    public void Write(ReadOnlySpan<byte> bytes)
    {
        byte[] frame = bytes.ToArray();
        _device.Frames.Add(frame);
        if (_baud != _device.Baud)
        {
            return;
        }

        foreach (byte value in _device.Answer(frame))
        {
            _replies.Enqueue(value);
        }
    }

    public int Read(Span<byte> buffer, TimeSpan timeout)
    {
        int count = 0;
        while (count < buffer.Length && _replies.Count > 0)
        {
            buffer[count++] = _replies.Dequeue();
        }

        return count;
    }

    public void SetBaudRate(int baudRate) => _baud = baudRate;

    public void Dispose()
    {
    }
}
