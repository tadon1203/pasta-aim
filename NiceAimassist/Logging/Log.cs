using System.Runtime.CompilerServices;

namespace NiceAimassist.Logging;

public static class Log
{
    private const string FilePrefix = "nice-aimassist-";
    private const int KeptFiles = 10;

    private static volatile LogLevel _level = LogLevel.Info;
    private static volatile LogWriter? _file;

    public static LogLevel Level
    {
        get => _level;
        set => _level = value;
    }

    public static string? FilePath { get; private set; }

    public static bool IsEnabled(LogLevel level) => level <= _level;

    /// <summary>Starts writing every enabled message to a new file in <paramref name="directory"/>. Old files beyond the last few are deleted. If a failure occurs, the console is the only sink.</summary>
    public static void OpenFile(string directory)
    {
        try
        {
            Directory.CreateDirectory(directory);
            Prune(directory);
            string path = Path.Combine(directory, $"{FilePrefix}{DateTime.Now:yyyyMMdd-HHmmss}.log");
            _file = new LogWriter(path);
            FilePath = path;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            Warn($"the log file could not be opened in {directory}: {error.Message}");
        }
    }

    /// <summary>Writes the pending lines and closes the file. Call it before the process exits.</summary>
    public static void Close()
    {
        LogWriter? file = _file;
        _file = null;
        file?.Dispose();
    }

    public static void Error(string message) => Write(LogLevel.Error, message);

    public static void Warn(string message) => Write(LogLevel.Warn, message);

    public static void Info(string message) => Write(LogLevel.Info, message);

    public static void Debug(string message) => Write(LogLevel.Debug, message);

    public static void Trace(string message) => Write(LogLevel.Trace, message);

    // An interpolated message is formatted only when its level is enabled. The holes are still evaluated.
    // Guard the call when a hole does real work.
    public static void Error(ref LogHandler<ErrorLevel> handler) => Write(ref handler);

    public static void Warn(ref LogHandler<WarnLevel> handler) => Write(ref handler);

    public static void Info(ref LogHandler<InfoLevel> handler) => Write(ref handler);

    public static void Debug(ref LogHandler<DebugLevel> handler) => Write(ref handler);

    public static void Trace(ref LogHandler<TraceLevel> handler) => Write(ref handler);

    /// <summary>Names the level that a <see cref="LogHandler{TLevel}"/> writes at.</summary>
    public interface ILevel
    {
        static abstract LogLevel Value { get; }
    }

    public readonly struct ErrorLevel : ILevel
    {
        public static LogLevel Value => LogLevel.Error;
    }

    public readonly struct WarnLevel : ILevel
    {
        public static LogLevel Value => LogLevel.Warn;
    }

    public readonly struct InfoLevel : ILevel
    {
        public static LogLevel Value => LogLevel.Info;
    }

    public readonly struct DebugLevel : ILevel
    {
        public static LogLevel Value => LogLevel.Debug;
    }

    public readonly struct TraceLevel : ILevel
    {
        public static LogLevel Value => LogLevel.Trace;
    }

    /// <summary>The handler for an interpolated message at level <typeparamref name="TLevel"/>.</summary>
    [InterpolatedStringHandler]
    public ref struct LogHandler<TLevel> where TLevel : ILevel
    {
        // Used only while IsEnabled is true. The default value is never written to.
        private DefaultInterpolatedStringHandler _text;

        public LogHandler(int literalLength, int formattedCount)
        {
            IsEnabled = Log.IsEnabled(TLevel.Value);
            _text = IsEnabled ? new DefaultInterpolatedStringHandler(literalLength, formattedCount) : default;
        }

        public bool IsEnabled { get; }

        public void AppendLiteral(string value)
        {
            if (IsEnabled)
            {
                _text.AppendLiteral(value);
            }
        }

        public void AppendFormatted<T>(T value)
        {
            if (IsEnabled)
            {
                _text.AppendFormatted(value);
            }
        }

        public void AppendFormatted<T>(T value, string? format)
        {
            if (IsEnabled)
            {
                _text.AppendFormatted(value, format);
            }
        }

        public void AppendFormatted<T>(T value, int alignment)
        {
            if (IsEnabled)
            {
                _text.AppendFormatted(value, alignment);
            }
        }

        public void AppendFormatted<T>(T value, int alignment, string? format)
        {
            if (IsEnabled)
            {
                _text.AppendFormatted(value, alignment, format);
            }
        }

        internal string ToStringAndClear() => _text.ToStringAndClear();
    }

    private static void Write<TLevel>(ref LogHandler<TLevel> handler) where TLevel : ILevel
    {
        if (handler.IsEnabled)
        {
            Write(TLevel.Value, handler.ToStringAndClear());
        }
    }

    private static void Write(LogLevel level, string message)
    {
        if (!IsEnabled(level))
        {
            return;
        }

        _file?.Write($"{DateTime.Now:HH:mm:ss.fff} {level.ToString().ToUpperInvariant(),-5} [{Thread.CurrentThread.Name ?? "t" + Environment.CurrentManagedThreadId}] {message}");

        // Console writes block on a slow terminal. Therefore, only the quiet levels reach the console.
        switch (level)
        {
            case LogLevel.Error:
                Console.Error.WriteLine($"error: {message}");
                break;
            case LogLevel.Warn:
                Console.Error.WriteLine($"warn: {message}");
                break;
            case LogLevel.Info:
                Console.WriteLine(message);
                break;
        }
    }

    private static void Prune(string directory)
    {
        string[] old = [.. Directory.GetFiles(directory, FilePrefix + "*.log").Order(StringComparer.Ordinal)];
        foreach (string path in old.Take(Math.Max(0, old.Length - (KeptFiles - 1))))
        {
            try
            {
                File.Delete(path);
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            {
            }
        }
    }
}
