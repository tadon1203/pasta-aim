namespace Pstm.Mouse.Serial;

/// <summary>The byte stream to one device. A COM port implements it. Tests use a fake.</summary>
public interface IDevicePort : IDisposable
{
    void Write(ReadOnlySpan<byte> bytes);

    /// <summary>Returns the number of bytes read. Returns 0 when the timeout passes with no data.</summary>
    int Read(Span<byte> buffer, TimeSpan timeout);

    void SetBaudRate(int baudRate);
}
