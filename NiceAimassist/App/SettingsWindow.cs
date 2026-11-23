using System.Diagnostics;
using System.Drawing;
using System.Windows.Forms;
using NiceAimassist.Aim;
using NiceAimassist.Dataset;
using NiceAimassist.Settings;
using NiceAimassist.Detect;
using NiceAimassist.Loop;
using NiceAimassist.Loop.Timing;
using NiceAimassist.Hotkey;
using NiceAimassist.Shared.Logging;
using NiceAimassist.Mouse;
using Timer = System.Windows.Forms.Timer;

namespace NiceAimassist.App;

/// <summary>
/// Edits the settings through <see cref="SettingsStore.Save"/>, and shows every other change of the settings.
/// The window reads the loop only through <see cref="LoopStatus"/> and <see cref="IDatasetRecorder.Status"/>.
/// </summary>
internal sealed class SettingsWindow : Form
{
    private const int SaveDelayMs = 300;
    private const int KeyPollMs = 15;
    private const int StatusIntervalMs = 1000;
    private const string AddKeyText = "Add";
    private readonly SettingsStore _file;
    private readonly LoopStatus _loopStatus;
    private readonly IDatasetRecorder _recorder;
    private readonly ManualResetEventSlim _stopped;
    private readonly string _modelsDirectory;

    private readonly TableLayoutPanel _layout = new();
    private readonly ComboBox _model = new();
    private readonly ComboBox _logLevel = new();
    private readonly ListBox _hotkeys = new();
    private readonly Button _addKey = new();
    private readonly Button _removeKey = new();
    private readonly NumericUpDown _sensitivityX = new();
    private readonly NumericUpDown _sensitivityY = new();
    private readonly NumericUpDown _confidence = new();
    private readonly NumericUpDown _targetYOffset = new();
    private readonly NumericUpDown _maxDelta = new();
    private readonly CheckBox _prediction = new();
    private readonly NumericUpDown _countsPerPixel = new();
    private readonly NumericUpDown _velocitySmoothing = new();
    private readonly NumericUpDown _interpolationPercent = new();
    private readonly CheckBox _interpolationAuto = new();
    private readonly CheckBox _datasetEnabled = new();
    private readonly Label _status = new();
    private readonly Timer _saveTimer = new();
    private readonly Timer _keyInputTimer = new();
    private readonly Timer _statusTimer = new();

    private RegisteredWaitHandle? _stopWait;
    private bool _armed;
    private bool _showing;
    private long _lastFrames;
    private long _lastTicks = Stopwatch.GetTimestamp();

    public SettingsWindow(
        SettingsStore file,
        LoopStatus loopStatus,
        IDatasetRecorder recorder,
        ManualResetEventSlim stopped,
        string modelsDirectory)
    {
        _file = file;
        _loopStatus = loopStatus;
        _recorder = recorder;
        _stopped = stopped;
        _modelsDirectory = modelsDirectory;

        InitializeComponent();
        LoadModels();
        ShowSettings(file.Current);
        WireEvents();
        _statusTimer.Start();
    }

    protected override void OnHandleCreated(EventArgs e)
    {
        base.OnHandleCreated(e);
        // A change between the constructor and the handle would otherwise be missed.
        ShowSettings(_file.Current);
        _file.Changed += OnSettingsChanged;
        _stopWait = ThreadPool.RegisterWaitForSingleObject(
            _stopped.WaitHandle, (_, _) => PostToWindow(Close), null, Timeout.Infinite, executeOnlyOnce: true);
    }

    protected override void Dispose(bool disposing)
    {
        if (disposing)
        {
            _file.Changed -= OnSettingsChanged;
            _stopWait?.Unregister(null);
            _saveTimer.Dispose();
            _keyInputTimer.Dispose();
            _statusTimer.Dispose();
        }

        base.Dispose(disposing);
    }

    private void InitializeComponent()
    {
        Text = "nice-aimassist";
        AutoSize = true;
        AutoSizeMode = AutoSizeMode.GrowAndShrink;
        FormBorderStyle = FormBorderStyle.FixedSingle;
        MaximizeBox = false;

        _layout.AutoSize = true;
        _layout.ColumnCount = 1;
        _layout.Dock = DockStyle.Fill;
        _layout.Padding = new Padding(12);

        _model.DropDownStyle = ComboBoxStyle.DropDownList;
        _model.Width = 220;

        _hotkeys.Width = 140;
        _hotkeys.Height = 60;
        _addKey.Text = AddKeyText;
        _removeKey.Text = "Remove";
        FlowLayoutPanel keyButtons = new() { AutoSize = true, FlowDirection = FlowDirection.TopDown };
        keyButtons.Controls.Add(_addKey);
        keyButtons.Controls.Add(_removeKey);
        FlowLayoutPanel keys = new() { AutoSize = true };
        keys.Controls.Add(_hotkeys);
        keys.Controls.Add(keyButtons);

        ConfigureNumber(_sensitivityX, AimSettings.MinSensitivity, AimSettings.MaxSensitivity);
        ConfigureNumber(_sensitivityY, AimSettings.MinSensitivity, AimSettings.MaxSensitivity);
        ConfigureNumber(_confidence, 0, DetectSettings.MaxConfidence);
        ConfigureNumber(_targetYOffset, 0, AimSettings.MaxTargetYOffset);
        ConfigureNumber(_maxDelta, 0, AimSettings.MaxDeltaLimit);
        ConfigureNumber(_countsPerPixel, AimSettings.MinSensitivity, AimSettings.MaxSensitivity);
        ConfigureNumber(_velocitySmoothing, 0, AimSettings.MaxVelocitySmoothingMs);
        ConfigureNumber(_interpolationPercent, 0, 100);
        _interpolationAuto.Text = "Auto";
        _interpolationAuto.AutoSize = true;
        FlowLayoutPanel interpolation = new() { AutoSize = true };
        interpolation.Controls.Add(_interpolationPercent);
        interpolation.Controls.Add(_interpolationAuto);

        _logLevel.DropDownStyle = ComboBoxStyle.DropDownList;
        _logLevel.Items.AddRange([.. Enum.GetValues<LogLevel>().Cast<object>()]);

        _prediction.Text = "Lead moving targets";
        _prediction.AutoSize = true;
        _datasetEnabled.Text = "Enabled (while hotkey is down)";
        _datasetEnabled.AutoSize = true;

        _status.AutoSize = true;
        _layout.Controls.Add(Rows(
            ("Model (restart)", _model),
            ("Confidence %", _confidence),
            ("Sensitivity X", _sensitivityX),
            ("Sensitivity Y", _sensitivityY),
            ("Target Y offset %", _targetYOffset),
            ("Max delta (0 = no cap)", _maxDelta),
            ("Prediction", _prediction),
            ("Prediction smoothing (ms)", _velocitySmoothing),
            ("Prediction counts per pixel %", _countsPerPixel),
            ("Interpolation %", interpolation),
            ("Hotkeys", keys),
            ("Dataset", _datasetEnabled),
            ("Log level", _logLevel)));
        _layout.Controls.Add(_status);
        Controls.Add(_layout);

        _saveTimer.Interval = SaveDelayMs;
        _keyInputTimer.Interval = KeyPollMs;
        _statusTimer.Interval = StatusIntervalMs;
    }

    private static void ConfigureNumber(NumericUpDown input, int min, int max)
    {
        input.Minimum = min;
        input.Maximum = max;
        input.Width = 80;
    }

    private static TableLayoutPanel Rows(params (string Text, Control Input)[] rows)
    {
        TableLayoutPanel table = new() { AutoSize = true, ColumnCount = 2 };
        foreach ((string text, Control input) in rows)
        {
            table.Controls.Add(new Label { Text = text, AutoSize = true, Anchor = AnchorStyles.Left, Margin = new Padding(3, 6, 12, 6) });
            input.Anchor = AnchorStyles.Left;
            table.Controls.Add(input);
        }

        return table;
    }

    private void LoadModels()
    {
        string[] models = Directory.Exists(_modelsDirectory)
            ? [.. Directory.GetFiles(_modelsDirectory, "*.onnx").Select(Path.GetFileName).OfType<string>().Order()]
            : [];
        _model.Items.AddRange(models);
    }

    // The control events fire while the controls change. They must not save what the settings already hold.
    private void ShowSettings(AppSettings settings)
    {
        _showing = true;
        try
        {
            if (settings.Detect.Model is { } current && !_model.Items.Contains(current))
            {
                _model.Items.Add(current);
            }

            _model.SelectedItem = settings.Detect.Model;
            string[] keys = [.. settings.Hotkey.Keys];
            if (!_hotkeys.Items.Cast<string>().SequenceEqual(keys))
            {
                _hotkeys.Items.Clear();
                _hotkeys.Items.AddRange(keys);
            }

            _sensitivityX.Value = settings.Aim.SensitivityX;
            _sensitivityY.Value = settings.Aim.SensitivityY;
            _confidence.Value = settings.Detect.Confidence;
            _targetYOffset.Value = settings.Aim.TargetYOffset;
            _maxDelta.Value = settings.Aim.MaxDelta;
            _prediction.Checked = settings.Aim.Prediction;
            _velocitySmoothing.Value = settings.Aim.VelocitySmoothingMs;
            _countsPerPixel.Value = settings.Aim.CountsPerPixel;
            _velocitySmoothing.Enabled = settings.Aim.Prediction;
            _countsPerPixel.Enabled = settings.Aim.Prediction;
            _interpolationAuto.Checked = settings.Mouse.Interpolation.IsAuto;
            _interpolationPercent.Value = settings.Mouse.Interpolation.FixedPercent ?? 0;
            _interpolationPercent.Enabled = !_interpolationAuto.Checked;
            _datasetEnabled.Checked = settings.Dataset.Enabled;
            _logLevel.SelectedItem = settings.Log.Level;
            UpdateKeyButtons();
        }
        finally
        {
            _showing = false;
        }
    }

    // A pending edit wins. The save that follows it shows its own result.
    private void OnSettingsChanged(AppSettings settings) =>
        PostToWindow(() =>
        {
            if (!_saveTimer.Enabled)
            {
                ShowSettings(settings);
            }
        });

    // The handle can be gone when the window closes while another thread posts.
    private void PostToWindow(Action action)
    {
        try
        {
            BeginInvoke(action);
        }
        catch (Exception error) when (error is InvalidOperationException or ObjectDisposedException)
        {
        }
    }

    private void WireEvents()
    {
        _model.SelectedIndexChanged += (_, _) => SettingChanged();
        _logLevel.SelectedIndexChanged += (_, _) => SettingChanged();
        _datasetEnabled.CheckedChanged += (_, _) => SettingChanged();
        foreach (NumericUpDown input in new[]
        {
            _sensitivityX, _sensitivityY, _confidence, _targetYOffset,
            _maxDelta, _countsPerPixel, _velocitySmoothing, _interpolationPercent,
        })
        {
            input.ValueChanged += (_, _) => SettingChanged();
        }

        _prediction.CheckedChanged += (_, _) =>
        {
            _velocitySmoothing.Enabled = _prediction.Checked;
            _countsPerPixel.Enabled = _prediction.Checked;
            SettingChanged();
        };
        _interpolationAuto.CheckedChanged += (_, _) =>
        {
            _interpolationPercent.Enabled = !_interpolationAuto.Checked;
            SettingChanged();
        };
        _hotkeys.SelectedIndexChanged += (_, _) => UpdateKeyButtons();
        _addKey.Click += (_, _) => BeginKey();
        _removeKey.Click += (_, _) => RemoveSelectedKey();
        _saveTimer.Tick += (_, _) => Flush();
        _keyInputTimer.Tick += (_, _) => PollKey();
        _statusTimer.Tick += (_, _) => RefreshStatus();
        FormClosing += (_, _) => Flush();
    }

    private AppSettings ReadSettings()
    {
        AppSettings current = _file.Current;
        return current with
        {
            Detect = current.Detect with
            {
                Model = _model.SelectedItem as string,
                Confidence = (int)_confidence.Value,
            },
            Aim = current.Aim with
            {
                SensitivityX = (int)_sensitivityX.Value,
                SensitivityY = (int)_sensitivityY.Value,
                TargetYOffset = (int)_targetYOffset.Value,
                MaxDelta = (int)_maxDelta.Value,
                Prediction = _prediction.Checked,
                CountsPerPixel = (int)_countsPerPixel.Value,
                VelocitySmoothingMs = (int)_velocitySmoothing.Value,
            },
            Hotkey = current.Hotkey with { Keys = [.. _hotkeys.Items.Cast<string>()] },
            Mouse = current.Mouse with
            {
                Interpolation = _interpolationAuto.Checked
                    ? Interpolation.Auto
                    : Interpolation.Percent((int)_interpolationPercent.Value),
            },
            Dataset = current.Dataset with
            {
                Enabled = _datasetEnabled.Checked,
            },
            Log = current.Log with
            {
                Level = _logLevel.SelectedItem is LogLevel level ? level : current.Log.Level,
            },
        };
    }

    private void SettingChanged()
    {
        if (_showing)
        {
            return;
        }

        _saveTimer.Stop();
        _saveTimer.Start();
    }

    private void Flush()
    {
        if (!_saveTimer.Enabled)
        {
            return;
        }

        _saveTimer.Stop();
        _file.Save(ReadSettings());
    }

    private void UpdateKeyButtons() =>
        _removeKey.Enabled = _hotkeys.SelectedIndex >= 0 && _hotkeys.Items.Count > 1;

    private void RemoveSelectedKey()
    {
        if (_hotkeys.SelectedIndex >= 0)
        {
            _hotkeys.Items.RemoveAt(_hotkeys.SelectedIndex);
            UpdateKeyButtons();
            SettingChanged();
        }
    }

    private void BeginKey()
    {
        _armed = false;
        _addKey.Enabled = false;
        _addKey.Text = "Press… (Esc)";
        _keyInputTimer.Start();
    }

    // The click that started the input is still down. Nothing counts until all keys are up once.
    private void PollKey()
    {
        if (Key.IsEscapeDown())
        {
            EndKey(null);
            return;
        }

        string? name = Key.FirstDown();
        if (name is null)
        {
            _armed = true;
        }
        else if (_armed)
        {
            EndKey(name);
        }
    }

    private void EndKey(string? name)
    {
        _keyInputTimer.Stop();
        _addKey.Enabled = true;
        _addKey.Text = AddKeyText;
        if (name is not null && !_hotkeys.Items.Cast<string>().Contains(name, StringComparer.OrdinalIgnoreCase))
        {
            _hotkeys.Items.Add(name);
            UpdateKeyButtons();
            SettingChanged();
        }
    }

    private void RefreshStatus()
    {
        long frames = _loopStatus.Frames;
        long ticks = Stopwatch.GetTimestamp();
        double fps = (frames - _lastFrames) * (double)Stopwatch.Frequency / (ticks - _lastTicks);
        FrameStages latency = _loopStatus.Latency.Average;
        _lastFrames = frames;
        _lastTicks = ticks;
        DatasetStatus datasetStatus = _recorder.Status;
        string dataset = datasetStatus.LastError is { } error
            ? $"   dataset error: {error}"
            : datasetStatus.Active ? $"   recording, saved {datasetStatus.Saved}" : string.Empty;
        _status.Text = $"fps {fps:F1}   latency {latency.FormatMs()}   "
            + $"{(_loopStatus.Held ? "held" : "idle")}{dataset}";
    }
}
