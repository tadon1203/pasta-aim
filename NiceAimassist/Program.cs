using System.Diagnostics;
using System.Runtime;
using System.Windows.Forms;
using NiceAimassist;
using NiceAimassist.Collect;
using NiceAimassist.Config;
using NiceAimassist.Detect;
using NiceAimassist.Display;
using NiceAimassist.Hold;
using NiceAimassist.Loop;
using NiceAimassist.Mouse;
using NiceAimassist.Ui;
using Application = System.Windows.Forms.Application;

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
        SettingsStore store = new(SettingsStore.DefaultPath);
        DisplayDevices? gpu = null;
        Collector? collector = null;
        IDetector? detector = null;
        MakcuDevice? device = null;
        FrameLoop? loop = null;
        ConsoleCancelEventHandler? onCancel = null;
        try
        {
            Settings startup = store.Current;
            Log.Level = startup.LogLevel;
            Log.Info($"log level {startup.LogLevel}; file {Log.FilePath ?? "(none)"}");
            ModelFolder.ModelLocation model = ModelFolder.Resolve(startup.Detect.Model);
            HoldKeySet holdKey = new(startup.Hold.Keys);

            gpu = new DisplayDevices(startup.Display.Adapter, startup.Display.Output);
            collector = new Collector(
                gpu, Path.Combine(AppContext.BaseDirectory, "dataset"), Stopwatch.GetTimestamp, Stopwatch.Frequency);
            detector = new Detector(gpu, model.FilePath, collector);
            device = MakcuDevice.Connect();
            Log.Info($"MAKCU on {device.PortName} at {device.BaudRate} baud");

            ManualResetEventSlim stopped = new();
            Exception? fault = null;
            // Only the loop thread reads this. The loop applies a new snapshot from the store once. It does not apply it on every loop.
            Settings? applied = null;
            loop = new FrameLoop(
                detector,
                device,
                holdKey.IsHeld,
                () =>
                {
                    Settings settings = store.Current;
                    if (!ReferenceEquals(settings, applied))
                    {
                        applied = settings;
                        Log.Level = settings.LogLevel;
                        holdKey.Update(settings.Hold.Keys);
                    }

                    return settings;
                },
                Stopwatch.GetTimestamp,
                error =>
                {
                    fault = error;
                    stopped.Set();
                },
                collector);

            onCancel = (_, args) =>
            {
                args.Cancel = true;
                stopped.Set();
            };
            Console.CancelKeyPress += onCancel;
            loop.Start();
            // Set the visual styles and DPI mode before the first window is created.
            Application.SetHighDpiMode(HighDpiMode.SystemAware);
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            using (SettingsForm form = new(store, loop.Stats, stopped, model.Directory))
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
            if (onCancel is not null)
            {
                Console.CancelKeyPress -= onCancel;
            }

            loop?.Stop();
            detector?.Dispose();
            collector?.Dispose();
            gpu?.Dispose();
            device?.Dispose();
            store.Dispose();
        }
    }
}
