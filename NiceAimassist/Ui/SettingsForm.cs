using System.Diagnostics;
using System.Drawing;
using System.Windows.Forms;
using NiceAimassist.Aim;
using NiceAimassist.Collect;
using NiceAimassist.Config;
using NiceAimassist.Detect;
using NiceAimassist.Loop;
using NiceAimassist.Hold;
using NiceAimassist.Mouse;
using Timer = System.Windows.Forms.Timer;

namespace NiceAimassist.Ui;

/// <summary>
/// Edits settings.json through <see cref="SettingsStore.Save"/>. The loop reads the change from the store.
/// The form shares no state with the loop, except <see cref="LoopStats"/>.
/// </summary>
internal sealed class SettingsForm : Form
{
    private const int SaveDelayMs = 300;
    private const int KeyInputPollMs = 15;
    private const int StatusIntervalMs = 1000;
    private const string AddKeyText = "Add";

    private readonly SettingsStore _store;
    private readonly LoopStats _stats;
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
    private readonly CheckBox _collectEnabled = new();
    private readonly Label _status = new();
    private readonly Timer _saveTimer = new();
    private readonly Timer _keyInputTimer = new();
    private readonly Timer _statusTimer = new();

    private bool _armed;
    private long _lastFrames;
    private long _lastTicks = Stopwatch.GetTimestamp();

    public SettingsForm(SettingsStore store, LoopStats stats, ManualResetEventSlim stopped, string modelsDirectory)
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

        ConfigureNumber(_sensitivityX, AimSettings.MinSensitivity, AimSettings.MaxSensitivity);
        ConfigureNumber(_sensitivityY, AimSettings.MinSensitivity, AimSettings.MaxSensitivity);
        ConfigureNumber(_confidence, 0, DetectSettings.MaxConfidence);
        ConfigureNumber(_targetYOffset, 0, AimSettings.MaxTargetYOffset);
        ConfigureNumber(_maxDelta, 0, AimSettings.MaxDeltaLimit);
        ConfigureNumber(_velocitySmoothing, 0, AimSettings.MaxVelocitySmoothing);
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

        ConfigureNumber(_collectSize, CollectSettings.MinSize, CollectSettings.MaxSize);
        ConfigureNumber(_collectRate, CollectSettings.MinRate, CollectSettings.MaxRate);
        _collectEnabled.Text = "Enabled (while hold key is down)";
        _collectEnabled.AutoSize = true;
        AddRow("Collect", _collectEnabled);
        AddRow("Collect size px", _collectSize);
        AddRow("Collect images/s", _collectRate);

        _status.AutoSize = true;
        _layout.Controls.Add(_status);
        _layout.SetColumnSpan(_status, 2);
        Controls.Add(_layout);

        _saveTimer.Interval = SaveDelayMs;
        _keyInputTimer.Interval = KeyInputPollMs;
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
        if (settings.Detect.Model is { } current && !_model.Items.Contains(current))
        {
            _model.Items.Add(current);
        }

        _model.SelectedItem = settings.Detect.Model;
        _holdKeys.Items.AddRange([.. settings.Hold.Keys]);
        _sensitivityX.Value = settings.Aim.SensitivityX;
        _sensitivityY.Value = settings.Aim.SensitivityY;
        _confidence.Value = settings.Detect.Confidence;
        _targetYOffset.Value = settings.Aim.TargetYOffset;
        _maxDelta.Value = settings.Aim.MaxDelta;
        _velocitySmoothing.Value = settings.Aim.VelocitySmoothing;
        _interpolationAuto.Checked = settings.Mouse.Interpolation.IsAuto;
        _interpolationPercent.Value = settings.Mouse.Interpolation.FixedPercent ?? 0;
        _interpolationPercent.Enabled = !_interpolationAuto.Checked;
        _collectSize.Value = settings.Collect.Size;
        _collectRate.Value = settings.Collect.Rate;
        _collectEnabled.Checked = settings.Collect.Enabled;
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
        _addKey.Click += (_, _) => BeginKeyInput();
        _removeKey.Click += (_, _) => RemoveSelectedKey();
        _saveTimer.Tick += (_, _) => Flush();
        _keyInputTimer.Tick += (_, _) => PollKeyInput();
        _statusTimer.Tick += (_, _) => RefreshStatus();
        FormClosing += (_, _) => Flush();
    }

    private Settings ReadSettings() => _store.Current with
    {
        Detect = _store.Current.Detect with
        {
            Model = _model.SelectedItem as string,
            Confidence = (int)_confidence.Value,
        },
        Aim = _store.Current.Aim with
        {
            SensitivityX = (int)_sensitivityX.Value,
            SensitivityY = (int)_sensitivityY.Value,
            TargetYOffset = (int)_targetYOffset.Value,
            MaxDelta = (int)_maxDelta.Value,
            VelocitySmoothing = (int)_velocitySmoothing.Value,
        },
        Hold = _store.Current.Hold with { Keys = [.. _holdKeys.Items.Cast<string>()] },
        Mouse = _store.Current.Mouse with
        {
            Interpolation = _interpolationAuto.Checked
                ? Interpolation.Auto
                : Interpolation.Percent((int)_interpolationPercent.Value),
        },
        Collect = _store.Current.Collect with
        {
            Size = (int)_collectSize.Value,
            Rate = (int)_collectRate.Value,
            Enabled = _collectEnabled.Checked,
        },
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

    private void BeginKeyInput()
    {
        _armed = false;
        _addKey.Enabled = false;
        _addKey.Text = "Press… (Esc)";
        _keyInputTimer.Start();
    }

    // The click that started the input is still down. Nothing counts until all keys are up once.
    private void PollKeyInput()
    {
        if (HoldKey.IsEscapeDown())
        {
            EndKeyInput(null);
            return;
        }

        string? name = HoldKey.FirstDown();
        if (name is null)
        {
            _armed = true;
        }
        else if (_armed)
        {
            EndKeyInput(name);
        }
    }

    private void EndKeyInput(string? name)
    {
        _keyInputTimer.Stop();
        _addKey.Enabled = true;
        _addKey.Text = AddKeyText;
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
        StageTicks latency = _stats.Latency.Average;
        _lastFrames = frames;
        _lastTicks = ticks;
        CollectStatus collectStatus = _stats.Collect;
        string collect = collectStatus.LastError is { } error
            ? $"   collect error: {error}"
            : collectStatus.Active ? $"   collecting, saved {collectStatus.Saved}" : string.Empty;
        _status.Text = $"fps {fps:F1}   latency {latency.FormatMs()}   "
            + $"{(_stats.Held ? "held" : "idle")}{collect}";
    }
}
