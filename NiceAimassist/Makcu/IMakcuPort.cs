namespace NiceAimassist.Makcu;

/// <summary>The byte stream to one MAKCU device. Implemented over a COM port; tests use a fake.</summary>
public interface IMakcuPort : IDisposable
{
    void Write(ReadOnlySpan<byte> bytes);

    /// <summary>Returns the number of bytes read, or 0 when the timeout passes with no data.</summary>
    int Read(Span<byte> buffer, TimeSpan timeout);

    void SetBaudRate(int baudRate);
}
