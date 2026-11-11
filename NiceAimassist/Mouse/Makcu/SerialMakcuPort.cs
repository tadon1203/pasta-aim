using System.IO.Ports;

namespace NiceAimassist.Mouse.Makcu;

internal sealed class SerialMakcuPort : IMakcuPort
{
    private const int WriteTimeoutMs = 50;
    private const int WriteBufferSize = 64;

    // The device applies a baud switch after the SET_BAUD packet leaves the UART.
    // Changing the host rate earlier garbles the packet.
    private static readonly TimeSpan BaudSwitchSettle = TimeSpan.FromMilliseconds(10);

    private readonly SerialPort _port;
    private readonly byte[] _write = new byte[MakcuProtocol.MaxRequestLength];
    private readonly byte[] _read = new byte[256];

    private SerialMakcuPort(string name, int baudRate)
    {
        _port = new SerialPort(name, baudRate, Parity.None, 8, StopBits.One)
        {
            Handshake = Handshake.None,
            WriteTimeout = WriteTimeoutMs,
            WriteBufferSize = WriteBufferSize,
        };
        _port.Open();
        _port.DiscardInBuffer();
    }

    public static IMakcuPort Open(string name, int baudRate) => new SerialMakcuPort(name, baudRate);

    public void Write(ReadOnlySpan<byte> bytes)
    {
        bytes.CopyTo(_write);
        _port.Write(_write, 0, bytes.Length);
    }

    public int Read(Span<byte> buffer, TimeSpan timeout)
    {
        _port.ReadTimeout = Math.Max(1, (int)timeout.TotalMilliseconds);
        try
        {
            int count = _port.Read(_read, 0, Math.Min(buffer.Length, _read.Length));
            _read.AsSpan(0, count).CopyTo(buffer);
            return count;
        }
        catch (TimeoutException)
        {
            return 0;
        }
    }

    public void SetBaudRate(int baudRate)
    {
        Thread.Sleep(BaudSwitchSettle);
        _port.BaudRate = baudRate;
        _port.DiscardInBuffer();
    }

    public void Dispose() => _port.Dispose();
}
