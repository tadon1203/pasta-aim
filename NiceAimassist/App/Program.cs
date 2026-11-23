using System.Runtime;
using System.Windows.Forms;
using NiceAimassist.Detect.Model;
using NiceAimassist.Hotkey;
using NiceAimassist.Settings;
using NiceAimassist.Shared.Logging;
using Application = System.Windows.Forms.Application;

namespace NiceAimassist.App;

internal static class Program
{
    [STAThread]
    private static int Main()
    {
        GCSettings.LatencyMode = GCLatencyMode.SustainedLowLatency;
        Log.OpenFile(Path.Combine(AppContext.BaseDirectory, "logs"));
        try
        {
            return Run();
        }
        catch (Exception error)
        {
            Log.Error(error.Message);
            Log.Debug(error.ToString());
            return 1;
        }
        finally
        {
            Log.Close();
        }
    }

    private static int Run()
    {
        using SettingsStore file = new(SettingsStore.DefaultPath);
        AppSettings startup = file.Current;
        Log.Level = startup.Log.Level;
        Log.Info($"log level {startup.Log.Level}; file {Log.FilePath ?? "(none)"}");
        HotkeySet hotkey = new(startup.Hotkey.Keys);
        if (!hotkey.HasKeys)
        {
            throw new InvalidOperationException(
                $"settings.json has no known hotkey: {string.Join(", ", startup.Hotkey.Keys)}");
        }

        ManualResetEventSlim stopped = new();
        Exception? fault = null;
        using IDisposable applier = SettingsApplier.Subscribe(file, hotkey);
        using Pipeline pipeline = new(
            file,
            startup,
            hotkey,
            error =>
            {
                fault = error;
                stopped.Set();
            });

        ConsoleCancelEventHandler onCancel = (_, args) =>
        {
            args.Cancel = true;
            stopped.Set();
        };

        try
        {
            Console.CancelKeyPress += onCancel;
            pipeline.Start();

            // Set the visual styles and DPI mode before the first window is created.
            Application.SetHighDpiMode(HighDpiMode.SystemAware);
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            using (SettingsWindow form = new(file, pipeline.Loop.LoopStatus, pipeline.Recorder, stopped, ModelFolder.Location))
            {
                Application.Run(form);
            }

            if (fault is not null)
            {
                throw new InvalidOperationException($"the loop stopped: {fault.Message}", fault);
            }

            return 0;
        }
        finally
        {
            Console.CancelKeyPress -= onCancel;
        }
    }
}
