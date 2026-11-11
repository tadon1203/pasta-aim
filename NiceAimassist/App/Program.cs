using System.Diagnostics;
using System.Runtime;
using System.Windows.Forms;
using NiceAimassist.Dataset;
using NiceAimassist.Settings;
using NiceAimassist.Detect;
using NiceAimassist.Capture;
using NiceAimassist.Detect.Model;
using NiceAimassist.Detect.Runtime;
using NiceAimassist.Shared.Gpu;
using NiceAimassist.Hotkey;
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
        OnnxSession? session = null;
        DesktopCapture? frames = null;
        DatasetRecorder? recorder = null;
        Detector? detector = null;
        MakcuDevice? device = null;
        FrameLoop? loop = null;
        ConsoleCancelEventHandler? onCancel = null;
        try
        {
            AppSettings startup = file.Current;
            Log.Level = startup.Log.Level;
            Log.Info($"log level {startup.Log.Level}; file {Log.FilePath ?? "(none)"}");
            Directory.CreateDirectory(ModelFolder.Location);
            string modelPath = ModelFolder.Resolve(startup.Detect.Model);
            HotkeySet hotkey = new(startup.Hotkey.Keys);
            if (!hotkey.HasKeys)
            {
                throw new InvalidOperationException(
                    $"settings.json has no known hotkey: {string.Join(", ", startup.Hotkey.Keys)}");
            }

            gpu = new GpuDevices(startup.Display.Adapter, startup.Display.Output);
            session = new OnnxSession(gpu, modelPath);
            frames = new DesktopCapture(gpu, session.Input.Width, session.Input.Height);
            recorder = new DatasetRecorder(
                gpu, Path.Combine(AppContext.BaseDirectory, "dataset"), Stopwatch.GetTimestamp, Stopwatch.Frequency);
            detector = new Detector(gpu, session, frames);
            Log.Info($"model {Path.GetFileName(modelPath)}");
            device = MakcuDevice.Connect();
            Log.Info($"MAKCU on {device.PortName} at {device.BaudRate} baud");

            ManualResetEventSlim stopped = new();
            Exception? fault = null;
            LiveSettings.Attach(file, hotkey);
            loop = new FrameLoop(
                frames,
                detector,
                device,
                hotkey.IsHeld,
                () => file.Current,
                Stopwatch.GetTimestamp,
                error =>
                {
                    fault = error;
                    stopped.Set();
                },
                recorder);

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
            using (MainWindow form = new(file, loop.LoopStatus, recorder, stopped, ModelFolder.Location))
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
            session?.Dispose();
            frames?.Dispose();
            recorder?.Dispose();
            gpu?.Dispose();
            device?.Dispose();
            file.Dispose();
        }
    }
}
