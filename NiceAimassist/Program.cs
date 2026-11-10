using System.Diagnostics;
using System.Runtime;
using NiceAimassist;
using NiceAimassist.Collect;
using NiceAimassist.Config;
using NiceAimassist.Engine;
using NiceAimassist.Gpu;
using NiceAimassist.Input;
using NiceAimassist.Makcu;
using NiceAimassist.Pipeline;
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
        GpuContext? gpu = null;
        Collector? collector = null;
        IFramePipeline? pipeline = null;
        MakcuDevice? device = null;
        Engine? engine = null;
        try
        {
            Settings startup = store.Current;
            Log.Level = startup.LogLevel;
            Log.Info($"log level {startup.LogLevel}; file {Log.FilePath ?? "(none)"}");
            string modelPath = ResolveModel(startup.Model);
            HoldKeySource holdKey = new(store.Current.HoldKeys, "hold");
            HoldKeySource collectKey = new(store.Current.CollectKeys, "collect");

            gpu = new GpuContext(startup.DisplayAdapter, startup.DisplayOutput);
            collector = new Collector(
                gpu, Path.Combine(AppContext.BaseDirectory, "dataset"), Stopwatch.GetTimestamp, Stopwatch.Frequency);
            pipeline = new FramePipeline(gpu, modelPath, collector);
            device = MakcuDevice.Connect();
            Log.Info($"MAKCU on {device.PortName} at {device.BaudRate} baud");

            ManualResetEventSlim stopped = new();
            Exception? fault = null;
            engine = new Engine(
                pipeline,
                new MakcuSink(device),
                holdKey.IsHeld,
                () =>
                {
                    Settings settings = store.Current;
                    Log.Level = settings.LogLevel;
                    holdKey.Follow(settings.HoldKeys);
                    collectKey.Follow(settings.CollectKeys);
                    return settings;
                },
                Stopwatch.GetTimestamp,
                error =>
                {
                    fault = error;
                    stopped.Set();
                },
                collector,
                collectKey.IsHeld);

            ConsoleCancelEventHandler onCancel = (_, args) =>
            {
                args.Cancel = true;
                stopped.Set();
            };
            Console.CancelKeyPress += onCancel;
            engine.Start();
            using (SettingsForm form = new(store, engine.Stats, stopped, Path.GetDirectoryName(modelPath)!))
            {
                Application.EnableVisualStyles();
                Application.Run(form);
            }

            Console.CancelKeyPress -= onCancel;

            if (fault is not null)
            {
                throw new InvalidOperationException($"the engine stopped: {fault.Message}", fault);
            }

            return 0;
        }
        finally
        {
            engine?.Stop();
            pipeline?.Dispose();
            collector?.Dispose();
            gpu?.Dispose();
            device?.Dispose();
            store.Dispose();
        }
    }

    private static string ResolveModel(string? model)
    {
        string directory = Path.Combine(AppContext.BaseDirectory, "models");
        Directory.CreateDirectory(directory);

        if (model is null)
        {
            throw new InvalidOperationException("no model is selected; set \"model\" in settings.json to a .onnx file name in models/");
        }

        string path = Path.Combine(directory, model);
        if (!File.Exists(path))
        {
            throw new InvalidOperationException($"model file not found: {path}");
        }

        return path;
    }

    private sealed record HoldBinding(IReadOnlyList<string> Names, HoldKey[] Keys);

    /// <summary>The hold keys. They are re-parsed on the engine thread when the settings file changes.</summary>
    private sealed class HoldKeySource
    {
        private HoldBinding _binding;

        private readonly string _role;

        public HoldKeySource(IReadOnlyList<string> names, string role)
        {
            _role = role;
            foreach (string name in names)
            {
                if (!HoldKey.TryParse(name, out _))
                {
                    throw new InvalidOperationException($"unknown {role} key \"{name}\" in settings.json");
                }
            }

            _binding = new HoldBinding(names, Parse(names));
        }

        public bool IsHeld()
        {
            foreach (HoldKey key in Volatile.Read(ref _binding).Keys)
            {
                if (key.IsDown())
                {
                    return true;
                }
            }

            return false;
        }

        public void Follow(IReadOnlyList<string> names)
        {
            HoldBinding current = Volatile.Read(ref _binding);
            if (current.Names.SequenceEqual(names))
            {
                return;
            }

            HoldKey[] keys = Parse(names);
            if (keys.Length == 0)
            {
                Volatile.Write(ref _binding, current with { Names = names });
                Log.Warn($"no known {_role} key; keeping the previous keys");
                return;
            }

            if (keys.Length < names.Count)
            {
                Log.Warn($"unknown {_role} keys are ignored");
            }

            Volatile.Write(ref _binding, new HoldBinding(names, keys));
        }

        private static HoldKey[] Parse(IReadOnlyList<string> names) =>
            [.. names.Select(name => HoldKey.TryParse(name, out HoldKey key) ? (HoldKey?)key : null)
                .OfType<HoldKey>()];
    }

    private sealed class MakcuSink(MakcuDevice link) : IMoveSink
    {
        public void Move(short dx, short dy) => link.Move(dx, dy);

        public void SetInterpolation(Interpolation interpolation) => link.SetInterpolation(interpolation);
    }
}
