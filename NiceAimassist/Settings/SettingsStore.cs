using System.Text.Json;
using NiceAimassist.Shared.Logging;

namespace NiceAimassist.Settings;

/// <summary>
/// Reads the settings file. Re-reads it whenever it changes.
/// Readers take <see cref="Current"/> without a lock. A gate serializes the changes.
/// </summary>
public sealed class SettingsStore : IDisposable
{
    private readonly string _path;
    private readonly TimeSpan _debounce;
    private readonly object _gate = new();
    private readonly Timer _timer;
    private readonly FileSystemWatcher _watcher;
    private readonly AppSettings _startup;
    private AppSettings _current;
    private bool _disposed;

    /// <summary>
    /// Reads the file. Creates it with the defaults when it is missing. Starts watching it.
    /// A file that cannot be parsed gives the defaults. The file stays unchanged.
    /// </summary>
    /// <param name="debounceMilliseconds">Delay after the last file event before the file is re-read.</param>
    public SettingsStore(string path, int debounceMilliseconds = 200)
    {
        _path = Path.GetFullPath(path);
        _debounce = TimeSpan.FromMilliseconds(debounceMilliseconds);
        _current = ReadOrCreate();
        _startup = _current;
        _timer = new Timer(_ => Reload(), null, Timeout.Infinite, Timeout.Infinite);
        _watcher = new FileSystemWatcher(Path.GetDirectoryName(_path)!, Path.GetFileName(_path))
        {
            NotifyFilter = NotifyFilters.FileName | NotifyFilters.LastWrite | NotifyFilters.Size,
        };
        // Editors save by writing a temporary file and renaming it. Therefore, Renamed and Created events matter too.
        _watcher.Changed += (_, _) => ScheduleReload();
        _watcher.Created += (_, _) => ScheduleReload();
        _watcher.Renamed += (_, _) => ScheduleReload();
        _watcher.EnableRaisingEvents = true;
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
        AppSettings clamped = settings.Clamped();
        Publish(clamped);
        try
        {
            File.WriteAllText(_path, clamped.ToJson());
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            Log.Warn($"settings could not be written to {_path}; they stay in effect until the app closes: {error.Message}");
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

    private AppSettings ReadOrCreate()
    {
        if (!File.Exists(_path))
        {
            AppSettings defaults = new();
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
            return AppSettings.Parse(File.ReadAllText(_path));
        }
        catch (Exception error) when (error is JsonException or IOException or UnauthorizedAccessException)
        {
            Log.Warn($"settings could not be read from {_path}; using the defaults: {error.Message}");
            return new AppSettings();
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
        AppSettings updated;
        try
        {
            updated = AppSettings.Parse(File.ReadAllText(_path));
        }
        catch (Exception error) when (error is JsonException or IOException or UnauthorizedAccessException)
        {
            Log.Warn($"settings reload failed; keeping the previous settings: {error.Message}");
            return;
        }

        if (Publish(updated))
        {
            Log.Info("settings reloaded");
        }
    }

    // The file watcher reads back what Save wrote. That read changes nothing and raises no event.
    private bool Publish(AppSettings updated)
    {
        AppSettings previous;
        lock (_gate)
        {
            previous = _current;
            if (_disposed || updated == previous)
            {
                return false;
            }

            Volatile.Write(ref _current, updated);
        }

        if (RestartValuesDiffer(previous, updated) && RestartValuesDiffer(_startup, updated))
        {
            Log.Warn("model and display take effect after a restart");
        }

        Changed?.Invoke(updated);
        return true;
    }

    private static bool RestartValuesDiffer(AppSettings first, AppSettings second) =>
        first.Detect.Model != second.Detect.Model || first.Display != second.Display;
}
