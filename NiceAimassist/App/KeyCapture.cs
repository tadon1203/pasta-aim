using System.Windows.Forms;
using NiceAimassist.Hotkey;
using Timer = System.Windows.Forms.Timer;

namespace NiceAimassist.App;

/// <summary>Shows the hotkeys and captures one key from the keyboard or the controller.</summary>
internal sealed class KeyCapture : IDisposable
{
    private const int PollMs = 15;
    private const string AddKeyText = "Add";

    private readonly ListBox _hotkeys;
    private readonly Button _addKey;
    private readonly Button _removeKey;
    private readonly Action _changed;
    private readonly Timer _timer = new();
    private bool _armed;

    public KeyCapture(ListBox hotkeys, Button add, Button remove, Action changed)
    {
        _hotkeys = hotkeys;
        _addKey = add;
        _removeKey = remove;
        _changed = changed;

        _addKey.Text = AddKeyText;
        _removeKey.Text = "Remove";
        _timer.Interval = PollMs;
        _hotkeys.SelectedIndexChanged += (_, _) => UpdateButtons();
        _addKey.Click += (_, _) => Begin();
        _removeKey.Click += (_, _) => RemoveSelected();
        _timer.Tick += (_, _) => Poll();
    }

    public string[] Keys => [.. _hotkeys.Items.Cast<string>()];

    public void SetKeys(IReadOnlyList<string> keys)
    {
        if (!_hotkeys.Items.Cast<string>().SequenceEqual(keys))
        {
            _hotkeys.Items.Clear();
            _hotkeys.Items.AddRange([.. keys]);
        }

        UpdateButtons();
    }

    public void Dispose() => _timer.Dispose();

    private void UpdateButtons() =>
        _removeKey.Enabled = _hotkeys.SelectedIndex >= 0 && _hotkeys.Items.Count > 1;

    private void RemoveSelected()
    {
        if (_hotkeys.SelectedIndex >= 0)
        {
            _hotkeys.Items.RemoveAt(_hotkeys.SelectedIndex);
            UpdateButtons();
            _changed();
        }
    }

    private void Begin()
    {
        _armed = false;
        _addKey.Enabled = false;
        _addKey.Text = "Press… (Esc)";
        _timer.Start();
    }

    // The click that started the input is still down. Nothing counts until all keys are up once.
    private void Poll()
    {
        if (Key.IsEscapeDown())
        {
            End(null);
            return;
        }

        string? name = Key.FirstDown();
        if (name is null)
        {
            _armed = true;
        }
        else if (_armed)
        {
            End(name);
        }
    }

    private void End(string? name)
    {
        _timer.Stop();
        _addKey.Enabled = true;
        _addKey.Text = AddKeyText;
        if (name is not null && !_hotkeys.Items.Cast<string>().Contains(name, StringComparer.OrdinalIgnoreCase))
        {
            _hotkeys.Items.Add(name);
            UpdateButtons();
            _changed();
        }
    }
}
