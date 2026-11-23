using NiceAimassist.Hotkey;
using NiceAimassist.Settings;
using NiceAimassist.Shared.Logging;

namespace NiceAimassist.App;

/// <summary>Applies the settings that act outside the loop, such as the log level and the hotkeys, when the settings change.</summary>
internal static class SettingsApplier
{
    public static void Attach(SettingsStore file, HotkeySet hotkeys) =>
        file.Changed += settings =>
        {
            Log.Level = settings.Log.Level;
            hotkeys.Update(settings.Hotkey.Keys);
        };
}
