using System.Text.Json;
using Pstm.Shared.Logging;

namespace Pstm.Settings;

/// <summary>
/// Holds the settings. Reads the file again when its text changes.
/// Readers take <see cref="Current"/> without a lock. A gate serializes the changes.
/// </summary>
public sealed class SettingsStore : IDisposable
{
    private readonly string _path;
    private readonly object _gate = new();
    private readonly Timer _timer;
    private readonly AppSettings _startup;
    private volatile string _text;
    private AppSettings _current;
    private bool _disposed;

    /// <summary>
    /// Reads the file. Creates it with the defaults when it is missing.
    /// A file that cannot be parsed gives the defaults. The file stays unchanged.
    /// </summary>
    /// <param name="pollMilliseconds">Time between two reads of the file.</param>
    public SettingsStore(string path, int pollMilliseconds = 500)
    {
        _path = Path.GetFullPath(path);
        _text = ReadOrCreate();
        _current = ParseOrDefaults(_text);
        _startup = _current;
        _timer = new Timer(_ => Refresh(), null, pollMilliseconds, pollMilliseconds);
    }

    /// <summary>Raised on the thread that changed the settings, after <see cref="Current"/> holds the new value.</summary>
    public event Action<AppSettings>? Changed;

    /// <summary>settings.json next to the executable.</summary>
    public static string DefaultPath => Path.Combine(AppContext.BaseDirectory, "settings.json");

    /// <summary>The latest settings. Safe to call from any thread.</summary>
    public AppSettings Current => Volatile.Read(ref _current);

    /// <summary>Makes the settings current at once. Then writes them to the file.</summary>
    public void Save(AppSettings settings)
    {
        string text = settings.Clamped().ToJson();
        Publish(text);
        try
        {
            File.WriteAllText(_path, text);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            Log.Warn($"settings could not be written to {_path}; they stay in effect until the app closes: {error.Message}");
        }
    }

    /// <summary>Reads the file and publishes a different text. The timer calls it.</summary>
    public void Refresh()
    {
        string text;
        try
        {
            text = File.ReadAllText(_path);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            return;
        }

        if (Publish(text))
        {
            Log.Info("settings reloaded");
        }
    }

    public void Dispose()
    {
        lock (_gate)
        {
            if (_disposed)
            {
                return;
            }

            _disposed = true;
        }

        _timer.Dispose();
    }

    private string ReadOrCreate()
    {
        if (!File.Exists(_path))
        {
            string defaults = new AppSettings().ToJson();
            try
            {
                File.WriteAllText(_path, defaults);
                Log.Info($"created {_path} with the default settings");
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            {
                Log.Warn($"could not write the default settings to {_path}: {error.Message}");
            }

            return defaults;
        }

        try
        {
            return File.ReadAllText(_path);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            Log.Warn($"settings could not be read from {_path}; using the defaults: {error.Message}");
            return new AppSettings().ToJson();
        }
    }

    private AppSettings ParseOrDefaults(string text)
    {
        try
        {
            return AppSettings.Parse(text);
        }
        catch (JsonException error)
        {
            Log.Warn($"settings could not be read from {_path}; using the defaults: {error.Message}");
            return new AppSettings();
        }
    }

    // The file text is the identity of a version. A save writes the text that it publishes, so a later read changes nothing.
    private bool Publish(string text)
    {
        AppSettings updated;
        try
        {
            updated = AppSettings.Parse(text);
        }
        catch (JsonException error)
        {
            Consume(text);
            Log.Warn($"settings reload failed; keeping the previous settings: {error.Message}");
            return false;
        }

        AppSettings previous;
        lock (_gate)
        {
            if (_disposed || text == _text)
            {
                return false;
            }

            previous = _current;
            _text = text;
            Volatile.Write(ref _current, updated);
        }

        if (RestartValuesDiffer(previous, updated) && RestartValuesDiffer(_startup, updated))
        {
            Log.Warn("model and display take effect after a restart");
        }

        Changed?.Invoke(updated);
        return true;
    }

    // A bad text stays consumed, so the next poll does not log the same error again.
    private void Consume(string text)
    {
        lock (_gate)
        {
            if (!_disposed)
            {
                _text = text;
            }
        }
    }

    private static bool RestartValuesDiffer(AppSettings first, AppSettings second) =>
        first.Detect.Model != second.Detect.Model || first.Display != second.Display;
}
