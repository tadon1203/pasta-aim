using System.Diagnostics;
using System.Runtime;
using System.Windows.Forms;
using NiceAimassist.Collect;
using NiceAimassist.Settings;
using NiceAimassist.Detect;
using NiceAimassist.Detect.Model;
using NiceAimassist.Shared.Gpu;
using NiceAimassist.Hold;
using NiceAimassist.Shared.Logging;
using NiceAimassist.Loop;
using NiceAimassist.Mouse;
using NiceAimassist.Mouse.Makcu;
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
            RaiseProcessPriority();
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

    private static void RaiseProcessPriority()
    {
        using Process process = Process.GetCurrentProcess();
        try
        {
            process.PriorityClass = ProcessPriorityClass.High;
        }
        catch (System.ComponentModel.Win32Exception error)
        {
            Log.Warn($"the process priority could not be set to High: {error.Message}");
        }
    }

    private static int Run()
    {
        SettingsFile file = new(SettingsFile.DefaultPath);
        GpuDevices? gpu = null;
        Collector? collector = null;
        IDetector? detector = null;
        MakcuDevice? device = null;
        FrameLoop? loop = null;
        ConsoleCancelEventHandler? onCancel = null;
        try
        {
            AppSettings startup = file.Current;
            Log.Level = startup.Log.Level;
            Log.Info($"log level {startup.Log.Level}; file {Log.FilePath ?? "(none)"}");
            ModelFolder.ModelLocation model = ModelFolder.Resolve(startup.Detect.Model);
            HoldKeySet holdKey = new(startup.Hold.Keys);

            gpu = new GpuDevices(startup.Display.Adapter, startup.Display.Output);
            collector = new Collector(
                gpu, Path.Combine(AppContext.BaseDirectory, "dataset"), Stopwatch.GetTimestamp, Stopwatch.Frequency);
            detector = new Detector(gpu, model.FilePath, collector);
            device = MakcuDevice.Connect();
            Log.Info($"MAKCU on {device.PortName} at {device.BaudRate} baud");

            ManualResetEventSlim stopped = new();
            Exception? fault = null;
            LiveSettings live = new(file, holdKey);
            loop = new FrameLoop(
                detector,
                device,
                holdKey.IsHeld,
                live.ApplyLatest,
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
            using (MainWindow form = new(file, loop.LoopStatus, stopped, model.Directory))
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
            file.Dispose();
        }
    }
}
