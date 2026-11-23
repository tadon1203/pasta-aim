using System.Buffers.Binary;
using Pstm.Mouse;
using Pstm.Mouse.Serial;

namespace Pstm.Tests.Mouse.Serial;

/// <summary>A simulated device. It answers only on a port whose baud rate matches its own. The device does the same.</summary>
internal sealed class FakeDevice
{
    public int Baud { get; set; } = 115_200;

    public HashSet<int> AcceptedBauds { get; } = [115_200, 4_000_000];

    public byte Kinds { get; set; } = 0x01;

    public List<byte[]> Frames { get; } = [];

    public FakeDevicePort Open(int baud) => new(this, baud);

    internal byte[] Answer(ReadOnlySpan<byte> frame)
    {
        byte command = frame[4];
        switch (command)
        {
            case DeviceProtocol.CommandDevice:
                return Reply(command, [Kinds]);
            case DeviceProtocol.CommandSmoothing:
                return Reply(command, [frame[5]]);
            case DeviceProtocol.CommandSetBaud:
                int baud = (int)BinaryPrimitives.ReadUInt32LittleEndian(frame[5..9]);
                if (AcceptedBauds.Contains(baud))
                {
                    Baud = baud;
                    return [];
                }

                return Reply(command, [DeviceProtocol.Reject]);
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

internal sealed class FakeDevicePort : IDevicePort
{
    private readonly FakeDevice _device;
    private readonly Queue<byte> _replies = new();
    private int _baud;

    internal FakeDevicePort(FakeDevice device, int baud)
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
