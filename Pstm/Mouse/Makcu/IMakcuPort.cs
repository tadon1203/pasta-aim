namespace Pstm.Mouse.Makcu;

/// <summary>The byte stream to one MAKCU device. A COM port implements it. Tests use a fake.</summary>
public interface IMakcuPort : IDisposable
{
    void Write(ReadOnlySpan<byte> bytes);

    /// <summary>Returns the number of bytes read. Returns 0 when the timeout passes with no data.</summary>
    int Read(Span<byte> buffer, TimeSpan timeout);

    void SetBaudRate(int baudRate);
}
