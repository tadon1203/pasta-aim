using System.Runtime;
using System.Windows.Forms;
using Pstm.Detect.Model;
using Pstm.Hotkey;
using Pstm.Settings;
using Pstm.Shared;
using Pstm.Shared.Logging;
using Application = System.Windows.Forms.Application;

namespace Pstm.App;

internal static class Program
{
    [STAThread]
    private static int Main()
    {
        using Mutex mutex = new(true, @"Local\pstm", out bool createdNew);
        if (!createdNew)
        {
            return 0;
        }

        GCSettings.LatencyMode = GCLatencyMode.SustainedLowLatency;
        Log.OpenFile(AppFolders.Logs);
        try
        {
            return Run();
        }
        catch (Exception error)
        {
            Log.Error(error.Message);
            Log.Debug(error.ToString());
            MessageBox.Show(error.Message, "pstm", MessageBoxButtons.OK, MessageBoxIcon.Error);
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
        AppSettings startup = SelectStartupModel(file);
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
        using IDisposable applier = SettingsApplier.Subscribe(file, hotkey, startup);
        using Pipeline pipeline = new(
            file,
            startup,
            hotkey,
            error =>
            {
                fault = error;
                stopped.Set();
            });

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

    // The settings window and the next start read the file, so the model name that the app uses is written back.
    private static AppSettings SelectStartupModel(SettingsStore file)
    {
        AppSettings settings = file.Current;
        if (settings.Detect.Model is not null)
        {
            return settings;
        }

        settings = settings with
        {
            Detect = settings.Detect with { Model = ModelFolder.FirstModelName() },
        };
        file.Save(settings);
        return settings;
    }
}
