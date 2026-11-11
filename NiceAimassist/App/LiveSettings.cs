using NiceAimassist.Hold;
using NiceAimassist.Logging;
using NiceAimassist.Settings;

namespace NiceAimassist.App;

/// <summary>
/// Gives the loop the latest settings. A new snapshot applies the log level and the hold keys once.
/// Only the loop thread calls <see cref="Current"/>. The loop applies a new snapshot once. It does not apply it on every loop.
/// </summary>
internal sealed class LiveSettings
{
    private readonly SettingsFile _file;
    private readonly HoldKeySet _holdKeys;
    private AppSettings? _applied;

    public LiveSettings(SettingsFile file, HoldKeySet holdKeys)
    {
        _file = file;
        _holdKeys = holdKeys;
    }

    public AppSettings Current()
    {
        AppSettings settings = _file.Current;
        if (!ReferenceEquals(settings, _applied))
        {
            _applied = settings;
            Log.Level = settings.Log.Level;
            _holdKeys.Update(settings.Hold.Keys);
        }

        return settings;
    }
}
