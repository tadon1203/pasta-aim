using Pstm.Hotkey;
using Pstm.Settings;
using Pstm.Shared.Logging;

namespace Pstm.App;

internal static class SettingsApplier
{
    /// <param name="startup">The settings of this run. The first change of a value that needs a restart warns.</param>
    public static IDisposable Subscribe(SettingsStore file, HotkeySet hotkeys, AppSettings startup)
    {
        AppSettings previous = startup;
        Action<AppSettings> handler = settings =>
        {
            Log.Level = settings.Log.Level;
            hotkeys.Update(settings.Hotkey.Keys);
            if (NeedsRestart(previous, settings) && NeedsRestart(startup, settings))
            {
                Log.Warn("model and display take effect after a restart");
            }

            previous = settings;
        };

        file.Changed += handler;
        return new Subscription(() => file.Changed -= handler);
    }

    private static bool NeedsRestart(AppSettings first, AppSettings second) =>
        first.Detect.Model != second.Detect.Model || first.Display != second.Display;

    private sealed class Subscription(Action release) : IDisposable
    {
        public void Dispose() => release();
    }
}
