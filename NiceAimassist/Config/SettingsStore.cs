using System.Text.Json;

namespace NiceAimassist.Config;

/// <summary>
/// Reads the settings file and re-reads it whenever it changes.
/// Readers take <see cref="Current"/> without a lock; reloads are serialized by a gate.
/// </summary>
public sealed class SettingsStore : IDisposable
{
    private readonly string _path;
    private readonly TimeSpan _debounce;
    private readonly object _gate = new();
    private readonly Timer _timer;
    private readonly FileSystemWatcher _watcher;
    private Settings _current;
    private bool _disposed;

    /// <summary>
    /// Reads the file, creates it with the defaults when it is missing, and starts watching it.
    /// An unparsable file gives the defaults and is left unchanged.
    /// </summary>
    /// <param name="debounceMilliseconds">Delay after the last file event before the file is re-read.</param>
    public SettingsStore(string path, int debounceMilliseconds = 200)
    {
        _path = Path.GetFullPath(path);
        _debounce = TimeSpan.FromMilliseconds(debounceMilliseconds);
        _current = ReadOrCreate();
        _timer = new Timer(_ => Reload(), null, Timeout.Infinite, Timeout.Infinite);
        _watcher = new FileSystemWatcher(Path.GetDirectoryName(_path)!, Path.GetFileName(_path))
        {
            NotifyFilter = NotifyFilters.FileName | NotifyFilters.LastWrite | NotifyFilters.Size,
        };
        // Editors save by writing a temporary file and renaming it, so Renamed and Created matter too.
        _watcher.Changed += (_, _) => ScheduleReload();
        _watcher.Created += (_, _) => ScheduleReload();
        _watcher.Renamed += (_, _) => ScheduleReload();
        _watcher.EnableRaisingEvents = true;
    }

    /// <summary>settings.json next to the executable.</summary>
    public static string DefaultPath => Path.Combine(AppContext.BaseDirectory, "settings.json");

    /// <summary>The latest settings. Safe to call from any thread.</summary>
    public Settings Current => Volatile.Read(ref _current);

    /// <summary>Writes the settings to the file. The watcher then reloads them into <see cref="Current"/>.</summary>
    public void Save(Settings settings)
    {
        try
        {
            File.WriteAllText(_path, settings.Validated().ToJson());
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            Log.Warn($"settings could not be written to {_path}: {error.Message}");
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

        _watcher.Dispose();
        _timer.Dispose();
    }

    private Settings ReadOrCreate()
    {
        if (!File.Exists(_path))
        {
            Settings defaults = new();
            try
            {
                File.WriteAllText(_path, defaults.ToJson());
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
            return Settings.Parse(File.ReadAllText(_path));
        }
        catch (Exception error) when (error is JsonException or IOException or UnauthorizedAccessException)
        {
            Log.Warn($"settings could not be read from {_path}; using the defaults: {error.Message}");
            return new Settings();
        }
    }

    private void ScheduleReload()
    {
        lock (_gate)
        {
            if (!_disposed)
            {
                _timer.Change(_debounce, Timeout.InfiniteTimeSpan);
            }
        }
    }

    private void Reload()
    {
        lock (_gate)
        {
            if (_disposed)
            {
                return;
            }

            Settings updated;
            try
            {
                updated = Settings.Parse(File.ReadAllText(_path));
            }
            catch (Exception error) when (error is JsonException or IOException or UnauthorizedAccessException)
            {
                Log.Warn($"settings reload failed; keeping the previous settings: {error.Message}");
                return;
            }

            Settings previous = _current;
            if (updated.Model != previous.Model
                || updated.DisplayAdapter != previous.DisplayAdapter
                || updated.DisplayOutput != previous.DisplayOutput)
            {
                Log.Warn("model, display_adapter and display_output take effect after a restart");
            }

            Volatile.Write(ref _current, updated);
            Log.Info("settings reloaded");
        }
    }
}
