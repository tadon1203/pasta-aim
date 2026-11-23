using System.Diagnostics;
using System.Windows.Forms;
using NiceAimassist.Dataset;
using NiceAimassist.Loop;
using NiceAimassist.Loop.Timing;
using Timer = System.Windows.Forms.Timer;

namespace NiceAimassist.App;

internal sealed class StatusLine : IDisposable
{
    private const int IntervalMs = 1000;

    private readonly Label _label;
    private readonly LoopStatus _loopStatus;
    private readonly IDatasetRecorder _recorder;
    private readonly Timer _timer = new();
    private long _frames;
    private long _ticks = Stopwatch.GetTimestamp();

    public StatusLine(Label label, LoopStatus loopStatus, IDatasetRecorder recorder)
    {
        _label = label;
        _loopStatus = loopStatus;
        _recorder = recorder;
        _timer.Interval = IntervalMs;
        _timer.Tick += (_, _) => Refresh();
    }

    public void Start() => _timer.Start();

    public void Dispose() => _timer.Dispose();

    private void Refresh()
    {
        long frames = _loopStatus.Frames;
        long ticks = Stopwatch.GetTimestamp();
        double fps = (frames - _frames) * (double)Stopwatch.Frequency / (ticks - _ticks);
        FrameStages latency = _loopStatus.Latency.Average;
        _frames = frames;
        _ticks = ticks;
        DatasetStatus datasetStatus = _recorder.Status;
        string dataset = datasetStatus.LastError is { } error
            ? $"   dataset error: {error}"
            : datasetStatus.Active ? $"   recording, saved {datasetStatus.Saved}" : string.Empty;
        _label.Text = $"fps {fps:F1}   latency {latency.FormatMs()}   "
            + $"{(_loopStatus.Held ? "held" : "idle")}{dataset}";
    }
}
