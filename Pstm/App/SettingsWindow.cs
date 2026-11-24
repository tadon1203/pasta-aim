using System.Drawing;
using System.Windows.Forms;
using Pstm.Correction;
using Pstm.Samples;
using Pstm.Settings;
using Pstm.Detect;
using Pstm.Loop;
using Pstm.Shared.Logging;
using Pstm.Mouse;
using Timer = System.Windows.Forms.Timer;

namespace Pstm.App;

/// <summary>
/// Edits the settings through <see cref="SettingsStore.Save"/>, and shows every other change of the settings.
/// The window reads the loop only through <see cref="LoopStatus"/> and <see cref="ISampleRecorder.Status"/>.
/// </summary>
internal sealed class SettingsWindow : Form
{
    private const int SaveDelayMs = 300;
    private readonly Font _sectionFont;
    private readonly SettingsStore _file;
    private readonly ManualResetEventSlim _stopped;
    private readonly string _modelsDirectory;
    private readonly KeyCapture _keyCapture;
    private readonly StatusLine _statusLine;

    private readonly TableLayoutPanel _layout = new();
    private readonly ComboBox _model = new();
    private readonly ComboBox _logLevel = new();
    private readonly ListBox _hotkeys = new();
    private readonly Button _addKey = new();
    private readonly Button _removeKey = new();
    private readonly NumericUpDown _sensitivityX = new();
    private readonly NumericUpDown _sensitivityY = new();
    private readonly NumericUpDown _threshold = new();
    private readonly NumericUpDown _offset = new();
    private readonly NumericUpDown _maxDelta = new();
    private readonly NumericUpDown _nearPx = new();
    private readonly NumericUpDown _nearBoost = new();
    private readonly NumericUpDown _smoothingPercent = new();
    private readonly CheckBox _smoothingAuto = new();
    private readonly CheckBox _samplesEnabled = new();
    private readonly Label _status = new();
    private readonly ToolTip _tips = new();
    private readonly Timer _saveTimer = new();

    private RegisteredWaitHandle? _stopWait;
    private bool _showing;

    public SettingsWindow(
        SettingsStore file,
        LoopStatus loopStatus,
        ISampleRecorder recorder,
        ManualResetEventSlim stopped,
        string modelsDirectory)
    {
        _file = file;
        _stopped = stopped;
        _modelsDirectory = modelsDirectory;

        _sectionFont = new Font(Font, FontStyle.Bold);
        InitializeComponent();
        _keyCapture = new KeyCapture(_hotkeys, _addKey, _removeKey, SettingChanged);
        _statusLine = new StatusLine(_status, loopStatus, recorder);
        LoadModels();
        ShowSettings(file.Current);
        WireEvents();
        _statusLine.Start();
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
            _tips.Dispose();
            _sectionFont.Dispose();
            _keyCapture.Dispose();
            _statusLine.Dispose();
        }

        base.Dispose(disposing);
    }

    private void InitializeComponent()
    {
        Text = "Settings";
        AutoSize = true;
        AutoSizeMode = AutoSizeMode.GrowAndShrink;
        FormBorderStyle = FormBorderStyle.FixedSingle;
        MaximizeBox = false;

        _layout.AutoSize = true;
        _layout.ColumnCount = 2;
        _layout.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        _layout.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        _layout.Dock = DockStyle.Fill;
        _layout.Padding = new Padding(12);

        _model.DropDownStyle = ComboBoxStyle.DropDownList;
        _model.Width = 220;

        _hotkeys.Width = 140;
        _hotkeys.Height = 60;
        FlowLayoutPanel keyButtons = new() { AutoSize = true, FlowDirection = FlowDirection.TopDown };
        keyButtons.Controls.Add(_addKey);
        keyButtons.Controls.Add(_removeKey);
        FlowLayoutPanel keys = new() { AutoSize = true };
        keys.Controls.Add(_hotkeys);
        keys.Controls.Add(keyButtons);

        ConfigureNumber(_sensitivityX, CorrectionSettings.MinSensitivity, CorrectionSettings.MaxSensitivity);
        ConfigureNumber(_sensitivityY, CorrectionSettings.MinSensitivity, CorrectionSettings.MaxSensitivity);
        ConfigureNumber(_threshold, 0, DetectSettings.MaxThreshold);
        ConfigureNumber(_offset, 0, CorrectionSettings.MaxOffset);
        ConfigureNumber(_maxDelta, 0, CorrectionSettings.MaxDeltaLimit);
        ConfigureNumber(_nearPx, 0, CorrectionSettings.MaxNearPx);
        ConfigureNumber(_nearBoost, 0, CorrectionSettings.MaxNearBoost);
        ConfigureNumber(_smoothingPercent, 0, 100);
        _smoothingAuto.Text = "Auto";
        _smoothingAuto.AutoSize = true;
        FlowLayoutPanel smoothing = new() { AutoSize = true };
        smoothing.Controls.Add(_smoothingPercent);
        smoothing.Controls.Add(_smoothingAuto);

        _logLevel.DropDownStyle = ComboBoxStyle.DropDownList;
        _logLevel.Items.AddRange([.. Enum.GetValues<LogLevel>().Cast<object>()]);

        _samplesEnabled.Text = "Enabled (while keys are down)";
        _samplesEnabled.AutoSize = true;

        _tips.SetToolTip(_nearPx, "Distance in input pixels where the near boost starts. 0 disables the boost.");
        _tips.SetToolTip(_nearBoost, "Sensitivity added at the target point. 0 keeps the base gain.");

        _status.AutoSize = true;
        AddSection("Input");
        AddRow("Model (restart)", _model);
        AddRow("Threshold %", _threshold);
        AddSection("Correction");
        AddRow("Gain X", _sensitivityX);
        AddRow("Gain Y", _sensitivityY);
        AddRow("Offset %", _offset);
        AddRow("Max step (0 = no cap)", _maxDelta);
        AddRow("Near radius (px)", _nearPx);
        AddRow("Near boost", _nearBoost);
        AddSection("Mouse");
        AddRow("Smoothing %", smoothing);
        AddSection("Keys");
        AddRow("Keys", keys);
        AddSection("Samples");
        AddRow("Sample recording", _samplesEnabled);
        AddSection("Log");
        AddRow("Log level", _logLevel);
        _layout.Controls.Add(_status);
        _layout.SetColumnSpan(_status, 2);
        Controls.Add(_layout);

        _saveTimer.Interval = SaveDelayMs;
    }

    private static void ConfigureNumber(NumericUpDown input, int min, int max)
    {
        input.Minimum = min;
        input.Maximum = max;
        input.Width = 80;
    }

    private void AddSection(string title)
    {
        Label label = new()
        {
            Text = title,
            AutoSize = true,
            Font = _sectionFont,
            Margin = new Padding(3, 12, 3, 3),
        };
        _layout.Controls.Add(label);
        _layout.SetColumnSpan(label, 2);
    }

    private void AddRow(string text, Control input)
    {
        _layout.Controls.Add(new Label
        {
            Text = text,
            AutoSize = true,
            Anchor = AnchorStyles.Left,
            Margin = new Padding(3, 6, 12, 6),
        });
        input.Anchor = AnchorStyles.Left;
        _layout.Controls.Add(input);
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
            _keyCapture.SetKeys(settings.Hotkey.Keys);
            _sensitivityX.Value = settings.Correction.SensitivityX;
            _sensitivityY.Value = settings.Correction.SensitivityY;
            _threshold.Value = settings.Detect.Threshold;
            _offset.Value = settings.Correction.Offset;
            _maxDelta.Value = settings.Correction.MaxDelta;
            _nearPx.Value = settings.Correction.NearPx;
            _nearBoost.Value = settings.Correction.NearBoost;
            _smoothingAuto.Checked = settings.Mouse.Smoothing.IsAuto;
            _smoothingPercent.Value = settings.Mouse.Smoothing.FixedPercent ?? 0;
            _smoothingPercent.Enabled = !_smoothingAuto.Checked;
            _samplesEnabled.Checked = settings.Samples.Enabled;
            _logLevel.SelectedItem = settings.Log.Level;
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
        _samplesEnabled.CheckedChanged += (_, _) => SettingChanged();
        foreach (NumericUpDown input in new[]
        {
            _sensitivityX, _sensitivityY, _threshold, _offset,
            _maxDelta, _nearPx, _nearBoost, _smoothingPercent,
        })
        {
            input.ValueChanged += (_, _) => SettingChanged();
        }

        _smoothingAuto.CheckedChanged += (_, _) =>
        {
            _smoothingPercent.Enabled = !_smoothingAuto.Checked;
            SettingChanged();
        };
        _saveTimer.Tick += (_, _) => Flush();
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
                Threshold = (int)_threshold.Value,
            },
            Correction = current.Correction with
            {
                SensitivityX = (int)_sensitivityX.Value,
                SensitivityY = (int)_sensitivityY.Value,
                Offset = (int)_offset.Value,
                MaxDelta = (int)_maxDelta.Value,
                NearPx = (int)_nearPx.Value,
                NearBoost = (int)_nearBoost.Value,
            },
            Hotkey = current.Hotkey with { Keys = _keyCapture.Keys },
            Mouse = current.Mouse with
            {
                Smoothing = _smoothingAuto.Checked
                    ? Smoothing.Auto
                    : Smoothing.Percent((int)_smoothingPercent.Value),
            },
            Samples = current.Samples with
            {
                Enabled = _samplesEnabled.Checked,
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
}
