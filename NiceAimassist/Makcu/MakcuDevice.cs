using System.Diagnostics;
using NiceAimassist.Config;

namespace NiceAimassist.Makcu;

/// <summary>An open link to a MAKCU device. Only the engine thread may use it.</summary>
public sealed class MakcuDevice : IDisposable
{
    private const int HighBaud = 4_000_000;
    private const int LowBaud = 115_200;
    private const byte AutoInterpolation = 255;
    private static readonly TimeSpan ReplyTimeout = TimeSpan.FromMilliseconds(100);

    private readonly IMakcuPort _port;
    private readonly byte[] _request = new byte[Protocol.MaxRequestLength];
    private bool _closed;

    private MakcuDevice(IMakcuPort port, string portName, int baudRate)
    {
        _port = port;
        PortName = portName;
        BaudRate = baudRate;
    }

    public string PortName { get; }

    public int BaudRate { get; }

    /// <exception cref="InvalidOperationException">No MAKCU answers, or the device has no mouse route.</exception>
    public static MakcuDevice Connect() => Connect(MakcuPorts.Candidates(), SerialMakcuPort.Open);

    internal static MakcuDevice Connect(IEnumerable<string> ports, Func<string, int, IMakcuPort> open)
    {
        var failures = new List<string>();
        foreach (string name in ports)
        {
            try
            {
                return ConnectPort(name, open);
            }
            catch (Exception error) when (IsConnectFailure(error))
            {
                failures.Add($"{name}: {error.Message}");
            }
        }

        throw new InvalidOperationException(
            failures.Count == 0 ? "no MAKCU port found" : string.Join("; ", failures));
    }

    /// <exception cref="ObjectDisposedException">The link is closed.</exception>
    /// <exception cref="IOException">The write failed. The link is closed.</exception>
    public void Move(short dx, short dy)
    {
        ThrowIfClosed();
        int length = Protocol.EncodeMove(_request, dx, dy);
        try
        {
            _port.Write(_request.AsSpan(0, length));
        }
        catch (Exception error) when (IsPortFailure(error))
        {
            Close();
            throw new IOException("MAKCU mouse move failed", error);
        }
    }

    /// <exception cref="InvalidOperationException">MAKCU rejected or answered the value differently. The link stays open.</exception>
    /// <exception cref="IOException">No reply came, or the port failed. The link is closed.</exception>
    public void SetInterpolation(Interpolation interpolation)
    {
        ThrowIfClosed();
        byte value = interpolation.FixedPercent is { } percent ? (byte)percent : AutoInterpolation;
        int length = Protocol.EncodeInterpolate(_request, value);

        Packet? reply;
        try
        {
            reply = Exchange(_port, _request.AsSpan(0, length), Protocol.CommandInterpolate);
        }
        catch (Exception error) when (IsPortFailure(error))
        {
            Close();
            throw new IOException("MAKCU did not confirm the move interpolation", error);
        }

        if (reply is null)
        {
            // A late reply would answer the next exchange, so the link is not kept.
            Close();
            throw new IOException("MAKCU did not confirm the move interpolation in time");
        }

        if (!Protocol.ConfirmsInterpolation(value, reply.Value))
        {
            throw new InvalidOperationException(
                $"MAKCU answered the move interpolation with {Convert.ToHexString(reply.Value.Payload)}");
        }
    }

    public void Dispose() => Close();

    private static MakcuDevice ConnectPort(string name, Func<string, int, IMakcuPort> open)
    {
        // The rate that the device keeps after the app closes is unknown, so 4 000 000 is tried first.
        IMakcuPort? port = null;
        try
        {
            port = open(name, HighBaud);
            int baud = HighBaud;
            byte? kinds = QueryKinds(port);
            if (kinds is null)
            {
                port.Dispose();
                port = null;
                port = open(name, LowBaud);
                kinds = QueryKinds(port)
                    ?? throw new InvalidOperationException("MAKCU did not answer at either baud rate");

                Span<byte> request = stackalloc byte[Protocol.MaxRequestLength];
                int length = Protocol.EncodeSetBaud(request, HighBaud);
                port.Write(request[..length]);
                port.SetBaudRate(HighBaud);
                baud = HighBaud;
                kinds = QueryKinds(port)
                    ?? throw new InvalidOperationException(
                        $"MAKCU did not answer at {HighBaud} baud after the baud switch");
            }

            if ((kinds.Value & Protocol.KindMouse) == 0)
            {
                throw new InvalidOperationException(
                    $"MAKCU on {name} has no mouse route (device kinds 0x{kinds.Value:X2})");
            }

            var link = new MakcuDevice(port, name, baud);
            port = null;
            return link;
        }
        finally
        {
            port?.Dispose();
        }
    }

    /// <summary>Null when the device does not answer. A reject is an error, not an answer.</summary>
    private static byte? QueryKinds(IMakcuPort port)
    {
        Span<byte> request = stackalloc byte[Protocol.MaxRequestLength];
        int length = Protocol.EncodeDevice(request);
        Packet? reply = Exchange(port, request[..length], Protocol.CommandDevice);
        if (reply is null)
        {
            return null;
        }

        if (Protocol.IsReject(reply.Value))
        {
            throw new InvalidOperationException("MAKCU rejected the device query");
        }

        if (reply.Value.Payload is not [var kinds])
        {
            throw new InvalidOperationException("MAKCU sent a malformed device reply");
        }

        return kinds;
    }

    private static Packet? Exchange(IMakcuPort port, ReadOnlySpan<byte> request, byte command)
    {
        port.Write(request);
        var decoder = new PacketDecoder();
        var packets = new List<Packet>();
        var buffer = new byte[256];
        var clock = Stopwatch.StartNew();
        while (clock.Elapsed < ReplyTimeout)
        {
            int count = port.Read(buffer, ReplyTimeout - clock.Elapsed);
            if (count == 0)
            {
                continue;
            }

            packets.Clear();
            decoder.Feed(buffer.AsSpan(0, count), packets);
            foreach (Packet packet in packets)
            {
                if (packet.Command == command)
                {
                    return packet;
                }
            }
        }

        return null;
    }

    private static bool IsConnectFailure(Exception error) =>
        error is IOException or TimeoutException or UnauthorizedAccessException or InvalidOperationException;

    private static bool IsPortFailure(Exception error) =>
        error is IOException or TimeoutException or InvalidOperationException;

    private void ThrowIfClosed()
    {
        if (_closed)
        {
            throw new ObjectDisposedException(nameof(MakcuDevice));
        }
    }

    private void Close()
    {
        if (_closed)
        {
            return;
        }

        _closed = true;
        _port.Dispose();
    }
}
