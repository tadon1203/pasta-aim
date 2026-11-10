using System.Diagnostics;
using System.Drawing;
using System.Windows.Forms;
using NiceAimassist.Config;
using NiceAimassist.Engine;
using NiceAimassist.Input;
using Timer = System.Windows.Forms.Timer;

namespace NiceAimassist.Ui;

/// <summary>
/// Edits settings.json through <see cref="SettingsStore.Save"/>. The engine picks the change up
/// from the store, so the form shares no state with it besides <see cref="EngineStats"/>.
/// </summary>
internal sealed class SettingsForm : Form
{
    private const int SaveDelayMs = 300;
    private const int CapturePollMs = 15;
    private const int StatusIntervalMs = 1000;
    private const string AddKeyText = "Add";

    private readonly SettingsStore _store;
    private readonly EngineStats _stats;
    private readonly ManualResetEventSlim _stopped;
    private readonly string _modelsDirectory;

    private readonly TableLayoutPanel _layout = new();
    private readonly ComboBox _model = new();
    private readonly ComboBox _logLevel = new();
    private readonly ListBox _holdKeys = new();
    private readonly Button _addKey = new();
    private readonly Button _removeKey = new();
    private readonly NumericUpDown _sensitivityX = new();
    private readonly NumericUpDown _sensitivityY = new();
    private readonly NumericUpDown _confidence = new();
    private readonly NumericUpDown _targetYOffset = new();
    private readonly NumericUpDown _maxDelta = new();
    private readonly NumericUpDown _velocitySmoothing = new();
    private readonly NumericUpDown _interpolationPercent = new();
    private readonly CheckBox _interpolationAuto = new();
    private readonly NumericUpDown _collectSize = new();
    private readonly NumericUpDown _collectRate = new();
    private readonly Button _collectKey = new();
    private readonly CheckBox _collectEnabled = new();
    private readonly Label _status = new();
    private readonly Timer _saveTimer = new();
    private readonly Timer _captureTimer = new();
    private readonly Timer _statusTimer = new();

    private bool _armed;
    private bool _capturingCollectKey;
    private long _lastFrames;
    private long _lastTicks = Stopwatch.GetTimestamp();

    public SettingsForm(SettingsStore store, EngineStats stats, ManualResetEventSlim stopped, string modelsDirectory)
    {
        _store = store;
        _stats = stats;
        _stopped = stopped;
        _modelsDirectory = modelsDirectory;

        InitializeComponent();
        LoadSettings(store.Current);
        WireEvents();
        _statusTimer.Start();
    }

    protected override void Dispose(bool disposing)
    {
        if (disposing)
        {
            _saveTimer.Dispose();
            _captureTimer.Dispose();
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
        _layout.ColumnCount = 2;
        _layout.Dock = DockStyle.Fill;
        _layout.Padding = new Padding(12);

        _model.DropDownStyle = ComboBoxStyle.DropDownList;
        _model.Width = 220;

        _holdKeys.Width = 140;
        _holdKeys.Height = 60;
        _addKey.Text = AddKeyText;
        _removeKey.Text = "Remove";
        FlowLayoutPanel keyButtons = new() { AutoSize = true, FlowDirection = FlowDirection.TopDown };
        keyButtons.Controls.Add(_addKey);
        keyButtons.Controls.Add(_removeKey);
        FlowLayoutPanel keys = new() { AutoSize = true };
        keys.Controls.Add(_holdKeys);
        keys.Controls.Add(keyButtons);

        ConfigureNumber(_sensitivityX, 1, 10_000);
        ConfigureNumber(_sensitivityY, 1, 10_000);
        ConfigureNumber(_confidence, 0, 100);
        ConfigureNumber(_targetYOffset, 0, 100);
        ConfigureNumber(_maxDelta, 0, 32_767);
        ConfigureNumber(_velocitySmoothing, 0, 95);
        ConfigureNumber(_interpolationPercent, 0, 100);
        _interpolationAuto.Text = "Auto";
        _interpolationAuto.AutoSize = true;
        FlowLayoutPanel interpolation = new() { AutoSize = true };
        interpolation.Controls.Add(_interpolationPercent);
        interpolation.Controls.Add(_interpolationAuto);

        AddRow("Model (restart)", _model);
        AddRow("Hold keys", keys);
        AddRow("Sensitivity X", _sensitivityX);
        AddRow("Sensitivity Y", _sensitivityY);
        AddRow("Confidence %", _confidence);
        AddRow("Target Y offset %", _targetYOffset);
        AddRow("Max delta (0 = no cap)", _maxDelta);
        AddRow("Velocity smoothing %", _velocitySmoothing);
        AddRow("Interpolation %", interpolation);

        _logLevel.DropDownStyle = ComboBoxStyle.DropDownList;
        _logLevel.Items.AddRange([.. Enum.GetValues<LogLevel>().Cast<object>()]);
        AddRow("Log level", _logLevel);

        ConfigureNumber(_collectSize, Settings.MinCollectSize, Settings.MaxCollectSize);
        ConfigureNumber(_collectRate, 1, Settings.MaxCollectRate);
        _collectKey.AutoSize = true;
        _collectEnabled.Text = "Enabled";
        _collectEnabled.AutoSize = true;
        AddRow("Collect", _collectEnabled);
        AddRow("Collect key (toggle)", _collectKey);
        AddRow("Collect size px", _collectSize);
        AddRow("Collect images/s", _collectRate);

        _status.AutoSize = true;
        _layout.Controls.Add(_status);
        _layout.SetColumnSpan(_status, 2);
        Controls.Add(_layout);

        _saveTimer.Interval = SaveDelayMs;
        _captureTimer.Interval = CapturePollMs;
        _statusTimer.Interval = StatusIntervalMs;
    }

    private static void ConfigureNumber(NumericUpDown input, int min, int max)
    {
        input.Minimum = min;
        input.Maximum = max;
        input.Width = 80;
    }

    private void AddRow(string text, Control input)
    {
        _layout.Controls.Add(new Label { Text = text, AutoSize = true, Anchor = AnchorStyles.Left, Margin = new Padding(3, 6, 12, 6) });
        input.Anchor = AnchorStyles.Left;
        _layout.Controls.Add(input);
    }

    private void LoadSettings(Settings settings)
    {
        string[] models = Directory.Exists(_modelsDirectory)
            ? [.. Directory.GetFiles(_modelsDirectory, "*.onnx").Select(Path.GetFileName).OfType<string>().Order()]
            : [];
        _model.Items.AddRange(models);
        if (settings.Model is { } current && !_model.Items.Contains(current))
        {
            _model.Items.Add(current);
        }

        _model.SelectedItem = settings.Model;
        _holdKeys.Items.AddRange([.. settings.HoldKeys]);
        _sensitivityX.Value = settings.SensitivityX;
        _sensitivityY.Value = settings.SensitivityY;
        _confidence.Value = settings.Confidence;
        _targetYOffset.Value = settings.TargetYOffset;
        _maxDelta.Value = settings.MaxDelta;
        _velocitySmoothing.Value = settings.VelocitySmoothing;
        _interpolationAuto.Checked = settings.Interpolation.IsAuto;
        _interpolationPercent.Value = settings.Interpolation.FixedPercent ?? 0;
        _interpolationPercent.Enabled = !_interpolationAuto.Checked;
        _collectSize.Value = settings.CollectSize;
        _collectRate.Value = settings.CollectRate;
        _collectKey.Text = settings.CollectKeys[0];
        _collectEnabled.Checked = settings.CollectEnabled;
        SetCollectInputsEnabled();
        _logLevel.SelectedItem = settings.LogLevel;
        UpdateKeyButtons();
    }

    private void WireEvents()
    {
        _model.SelectedIndexChanged += (_, _) => SettingChanged();
        _logLevel.SelectedIndexChanged += (_, _) => SettingChanged();
        _collectEnabled.CheckedChanged += (_, _) =>
        {
            SetCollectInputsEnabled();
            SettingChanged();
        };
        foreach (NumericUpDown input in new[]
        {
            _sensitivityX, _sensitivityY, _confidence, _targetYOffset,
            _maxDelta, _velocitySmoothing, _interpolationPercent, _collectSize, _collectRate,
        })
        {
            input.ValueChanged += (_, _) => SettingChanged();
        }

        _interpolationAuto.CheckedChanged += (_, _) =>
        {
            _interpolationPercent.Enabled = !_interpolationAuto.Checked;
            SettingChanged();
        };
        _holdKeys.SelectedIndexChanged += (_, _) => UpdateKeyButtons();
        _addKey.Click += (_, _) => BeginCapture(collectKey: false);
        _collectKey.Click += (_, _) => BeginCapture(collectKey: true);
        _removeKey.Click += (_, _) => RemoveSelectedKey();
        _saveTimer.Tick += (_, _) => Flush();
        _captureTimer.Tick += (_, _) => PollCapture();
        _statusTimer.Tick += (_, _) => RefreshStatus();
        FormClosing += (_, _) => Flush();
    }

    private Settings ReadSettings() => _store.Current with
    {
        Model = _model.SelectedItem as string,
        HoldKeys = [.. _holdKeys.Items.Cast<string>()],
        SensitivityX = (int)_sensitivityX.Value,
        SensitivityY = (int)_sensitivityY.Value,
        Confidence = (int)_confidence.Value,
        TargetYOffset = (int)_targetYOffset.Value,
        MaxDelta = (int)_maxDelta.Value,
        VelocitySmoothing = (int)_velocitySmoothing.Value,
        Interpolation = _interpolationAuto.Checked
            ? Interpolation.Auto
            : Interpolation.Percent((int)_interpolationPercent.Value),
        CollectSize = (int)_collectSize.Value,
        CollectRate = (int)_collectRate.Value,
        CollectKeys = [_collectKey.Text],
        CollectEnabled = _collectEnabled.Checked,
        LogLevel = _logLevel.SelectedItem is LogLevel level ? level : _store.Current.LogLevel,
    };

    private void SettingChanged()
    {
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
        _store.Save(ReadSettings());
    }

    private void SetCollectInputsEnabled()
    {
        bool enabled = _collectEnabled.Checked;
        _collectKey.Enabled = enabled;
        _collectSize.Enabled = enabled;
        _collectRate.Enabled = enabled;
    }

    private void UpdateKeyButtons() =>
        _removeKey.Enabled = _holdKeys.SelectedIndex >= 0 && _holdKeys.Items.Count > 1;

    private void RemoveSelectedKey()
    {
        if (_holdKeys.SelectedIndex >= 0)
        {
            _holdKeys.Items.RemoveAt(_holdKeys.SelectedIndex);
            UpdateKeyButtons();
            SettingChanged();
        }
    }

    private void BeginCapture(bool collectKey)
    {
        _armed = false;
        _capturingCollectKey = collectKey;
        Button button = collectKey ? _collectKey : _addKey;
        _addKey.Enabled = false;
        _collectKey.Enabled = false;
        button.Text = "Press… (Esc)";
        _captureTimer.Start();
    }

    // The click that started the capture is still down, so nothing counts until all keys are up once.
    private void PollCapture()
    {
        if (HoldKey.IsEscapeDown())
        {
            EndCapture(null);
            return;
        }

        string? name = HoldKey.FirstDown();
        if (name is null)
        {
            _armed = true;
        }
        else if (_armed)
        {
            EndCapture(name);
        }
    }

    private void EndCapture(string? name)
    {
        _captureTimer.Stop();
        _addKey.Enabled = true;
        _addKey.Text = AddKeyText;
        _collectKey.Enabled = true;
        if (_capturingCollectKey)
        {
            _collectKey.Text = name ?? _store.Current.CollectKeys[0];
            if (name is not null)
            {
                SettingChanged();
            }

            return;
        }

        if (name is not null && !_holdKeys.Items.Cast<string>().Contains(name, StringComparer.OrdinalIgnoreCase))
        {
            _holdKeys.Items.Add(name);
            UpdateKeyButtons();
            SettingChanged();
        }
    }

    private void RefreshStatus()
    {
        if (_stopped.IsSet)
        {
            Close();
            return;
        }

        long frames = _stats.Frames;
        long ticks = Stopwatch.GetTimestamp();
        double fps = (frames - _lastFrames) * (double)Stopwatch.Frequency / (ticks - _lastTicks);
        double latencyMs = _stats.LastLatencyTicks * 1000.0 / Stopwatch.Frequency;
        _lastFrames = frames;
        _lastTicks = ticks;
        string collect = _stats.CollectError is { } error
            ? $"   collect error: {error}"
            : _stats.Collecting ? $"   collecting, saved {_stats.Saved}" : string.Empty;
        _status.Text = $"fps {fps:F1}   latency {latencyMs:F2} ms   {(_stats.Held ? "held" : "idle")}{collect}";
    }
}
