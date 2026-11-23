namespace NiceAimassist.Shared.Logging;

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
