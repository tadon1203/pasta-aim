using NiceAimassist.Hotkey;
using NiceAimassist.Settings;
using NiceAimassist.Shared.Logging;

namespace NiceAimassist.App;

internal static class SettingsApplier
{
    public static IDisposable Subscribe(SettingsStore file, HotkeySet hotkeys)
    {
        Action<AppSettings> handler = settings =>
        {
            Log.Level = settings.Log.Level;
            hotkeys.Update(settings.Hotkey.Keys);
        };

        file.Changed += handler;
        return new Subscription(() => file.Changed -= handler);
    }

    private sealed class Subscription(Action release) : IDisposable
    {
        public void Dispose() => release();
    }
}
