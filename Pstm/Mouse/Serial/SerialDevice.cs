using System.Diagnostics;
using Pstm.Shared.Logging;

namespace Pstm.Mouse.Serial;

/// <summary>An open link to a device. Only the loop thread may use it.</summary>
public sealed class SerialDevice : IMouse, IDisposable
{
    private const int HighBaud = 4_000_000;
    private const int LowBaud = 115_200;
    private static readonly TimeSpan ReplyTimeout = TimeSpan.FromMilliseconds(100);

    private readonly IDevicePort _port;
    private readonly byte[] _request = new byte[DeviceProtocol.MaxRequestLength];
    private Smoothing? _confirmed;
    private bool _closed;

    private SerialDevice(IDevicePort port, string portName, int baudRate)
    {
        _port = port;
        PortName = portName;
        BaudRate = baudRate;
    }

    public string PortName { get; }

    public int BaudRate { get; }

    /// <exception cref="InvalidOperationException">No device answers, or the device has no mouse route.</exception>
    public static SerialDevice Connect() => Connect(DevicePorts.Candidates(), ComDevicePort.Open);

    internal static SerialDevice Connect(IEnumerable<string> ports, Func<string, int, IDevicePort> open)
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
            failures.Count == 0 ? "no device port found" : string.Join("; ", failures));
    }

    /// <exception cref="ObjectDisposedException">The link is closed.</exception>
    /// <exception cref="IOException">The write failed. The link is closed.</exception>
    public void Move(short dx, short dy)
    {
        ThrowIfClosed();
        int length = DeviceProtocol.EncodeMove(_request, dx, dy);
        try
        {
            _port.Write(_request.AsSpan(0, length));
        }
        catch (Exception error) when (IsPortFailure(error))
        {
            Close();
            throw new IOException("the device write failed", error);
        }
    }

    /// <summary>Sends the value only when it differs from the last confirmed value.</summary>
    /// <exception cref="InvalidOperationException">The device rejected or answered the value differently. The link stays open.</exception>
    /// <exception cref="IOException">No reply came, or the port failed. The link is closed.</exception>
    public void SetSmoothing(Smoothing smoothing)
    {
        ThrowIfClosed();
        if (_confirmed == smoothing)
        {
            return;
        }

        byte value = smoothing.FixedPercent is { } percent ? (byte)percent : DeviceProtocol.SmoothingAuto;
        int length = DeviceProtocol.EncodeSmoothing(_request, value);

        DevicePacket? reply;
        try
        {
            reply = Exchange(_port, _request.AsSpan(0, length), DeviceProtocol.CommandSmoothing);
        }
        catch (Exception error) when (IsPortFailure(error))
        {
            Close();
            throw new IOException("the device did not confirm the move smoothing", error);
        }

        if (reply is null)
        {
            // A late reply would answer the next exchange. Therefore, the link is closed.
            Close();
            throw new IOException("the device did not confirm the move smoothing in time");
        }

        if (!DeviceProtocol.ConfirmsSmoothing(value, reply.Value))
        {
            throw new InvalidOperationException(
                $"the device answered the move smoothing with {Convert.ToHexString(reply.Value.Payload)}");
        }

        _confirmed = smoothing;
        Log.Debug($"smoothing set to {(smoothing.FixedPercent is { } fixedPercent ? fixedPercent + "%" : "auto")}");
    }

    public void Dispose() => Close();

    private static SerialDevice ConnectPort(string name, Func<string, int, IDevicePort> open)
    {
        // The device keeps an unknown baud rate after the app closes. Therefore, 4 000 000 is tried first.
        IDevicePort? port = null;
        try
        {
            port = open(name, HighBaud);
            byte? kinds = QueryDeviceKinds(port);
            if (kinds is null)
            {
                port.Dispose();
                port = null;
                port = open(name, LowBaud);
                kinds = QueryDeviceKinds(port)
                    ?? throw new InvalidOperationException("the device did not answer at either baud rate");

                Span<byte> request = stackalloc byte[DeviceProtocol.MaxRequestLength];
                int length = DeviceProtocol.EncodeSetBaud(request, HighBaud);
                port.Write(request[..length]);
                port.SetBaudRate(HighBaud);
                kinds = QueryDeviceKinds(port)
                    ?? throw new InvalidOperationException(
                        $"the device did not answer at {HighBaud} baud after the baud switch");
            }

            if ((kinds.Value & DeviceProtocol.KindMouse) == 0)
            {
                throw new InvalidOperationException(
                    $"the device on {name} has no mouse route (kinds 0x{kinds.Value:X2})");
            }

            var link = new SerialDevice(port, name, HighBaud);
            port = null;
            return link;
        }
        finally
        {
            port?.Dispose();
        }
    }

    /// <summary>Null when the device does not answer. A reject is an error. It is not an answer.</summary>
    private static byte? QueryDeviceKinds(IDevicePort port)
    {
        Span<byte> request = stackalloc byte[DeviceProtocol.MaxRequestLength];
        int length = DeviceProtocol.EncodeDevice(request);
        DevicePacket? reply = Exchange(port, request[..length], DeviceProtocol.CommandDevice);
        if (reply is null)
        {
            return null;
        }

        if (DeviceProtocol.IsReject(reply.Value))
        {
            throw new InvalidOperationException("the device rejected the query");
        }

        if (reply.Value.Payload is not [var kinds])
        {
            throw new InvalidOperationException("the device sent a malformed reply");
        }

        return kinds;
    }

    private static DevicePacket? Exchange(IDevicePort port, ReadOnlySpan<byte> request, byte command)
    {
        port.Write(request);
        var decoder = new PacketDecoder();
        var packets = new List<DevicePacket>();
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
            foreach (DevicePacket packet in packets)
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
            throw new ObjectDisposedException(nameof(SerialDevice));
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
