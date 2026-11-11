using NiceAimassist.Hold;
using NiceAimassist.Logging;
using NiceAimassist.Settings;

namespace NiceAimassist.App;

/// <summary>
/// Gives the loop the latest settings. <see cref="ApplyLatest"/> applies the log level and the hold keys when the snapshot changes.
/// Only the loop thread calls <see cref="ApplyLatest"/>.
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

    public AppSettings ApplyLatest()
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
