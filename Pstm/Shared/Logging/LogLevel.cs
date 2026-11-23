namespace Pstm.Shared.Logging;

/// <summary>Ordered from the most to the least severe. A level enables itself and every level before it.</summary>
public enum LogLevel
{
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}
