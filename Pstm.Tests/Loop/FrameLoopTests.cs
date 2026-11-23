using System.Collections.Concurrent;
using System.Diagnostics;
using Pstm.Capture;
using Pstm.Samples;
using Pstm.Detect;
using Pstm.Loop;
using Pstm.Mouse;
using Pstm.Settings;
using Silk.NET.Direct3D11;

namespace Pstm.Tests.Loop;

public class FrameLoopTests
{
    // The target center is 100 px right of the input center (320). The default tracker moves dx = 100.
    private static readonly Detection Centered = new(400, 300, 440, 340, 0.9f, 0);

    [Fact]
    public void HeldKeyWithTargetSendsMove()
    {
        FakeFrameSource source = new();
        source.Enqueue(Stopwatch.GetTimestamp(), Centered);
        FakeMouse mouse = new();
        FrameLoop loop = Start(source, mouse, () => true);

        TestWait.Until(() => mouse.Moves.Count >= 1);
        loop.Stop();

        (short Dx, short Dy) move = Assert.Single(mouse.Moves);
        Assert.Equal((short)100, move.Dx);
        Assert.Equal((short)0, move.Dy);
    }

    [Fact]
    public void ReleasedKeySendsNoMoveAndDoesNotRunThePipeline()
    {
        FakeFrameSource source = new();
        source.Enqueue(Stopwatch.GetTimestamp(), Centered);
        FakeMouse mouse = new();
        FakeHotkey hotkey = new(false);
        FrameLoop loop = Start(source, mouse, hotkey.IsHeld);

        TestWait.Until(() => hotkey.Polls >= 3);
        loop.Stop();

        Assert.True(mouse.Moves.IsEmpty);
        Assert.Equal(0, source.AcquireCalls);
    }

    [Fact]
    public void ReleasingTheKeyResetsTheCorrectionTracker()
    {
        // Without the reset, the second frame reads the first move as target motion. The tracker then overshoots the target.
        long first = Stopwatch.GetTimestamp();
        FakeFrameSource source = new();
        source.Enqueue(first, Centered);
        source.Enqueue(first + Stopwatch.Frequency / 20, Centered);
        FakeMouse mouse = new();
        FakeHotkey hotkey = new(true, false, true);
        FrameLoop loop = Start(source, mouse, hotkey.IsHeld);

        TestWait.Until(() => mouse.Moves.Count >= 2);
        loop.Stop();

        (short Dx, short Dy)[] moves = mouse.Moves.ToArray();
        Assert.Equal((short)100, moves[0].Dx);
        Assert.Equal((short)100, moves[1].Dx);
    }

    [Fact]
    public void ReleasingTheKeyEndsTheHeldFrameOnce()
    {
        FakeFrameSource source = new();
        FakeHotkey hotkey = new(true, true, true, false);
        FrameLoop loop = Start(source, new FakeMouse(), hotkey.IsHeld);

        TestWait.Until(() => hotkey.Polls >= 8);
        loop.Stop();

        Assert.Equal(1, source.ReleaseCount);
    }

    [Fact]
    public void FrameWithoutTargetSendsNoMove()
    {
        FakeFrameSource source = new();
        source.Enqueue(Stopwatch.GetTimestamp(), null);
        FakeMouse mouse = new();
        FrameLoop loop = Start(source, mouse, () => true);

        TestWait.Until(() => loop.LoopStatus.Frames >= 1);
        loop.Stop();

        Assert.True(mouse.Moves.IsEmpty);
    }

    [Fact]
    public void TheMouseGetsTheSmoothingOfTheCurrentSettings()
    {
        // The frames are fed in two stages. The setting change happens between the stages.
        FakeFrameSource source = new();
        FakeMouse mouse = new();
        Holder<AppSettings> settings = new(Snapshot(Smoothing.Percent(30)));
        FrameLoop loop = NewLoop(source, mouse, () => true, () => settings.Value);
        loop.Start();

        source.Enqueue(Stopwatch.GetTimestamp(), null);
        TestWait.Until(() => loop.LoopStatus.Frames >= 1);
        Assert.Equal(Smoothing.Percent(30), mouse.Smoothings.Last());
        settings.Value = Snapshot(Smoothing.Auto);
        source.Enqueue(Stopwatch.GetTimestamp(), null);
        TestWait.Until(() => loop.LoopStatus.Frames >= 2);
        loop.Stop();

        Assert.Equal(Smoothing.Auto, mouse.Smoothings.Last());
    }

    [Fact]
    public void PipelineExceptionReportsFaultAndEndsTheLoop()
    {
        FakeFrameSource source = new() { Fault = new InvalidOperationException("device lost") };
        ConcurrentQueue<Exception> faults = new();
        FrameLoop loop = NewLoop(source, new FakeMouse(), () => true, Snapshot, faults.Enqueue);
        loop.Start();

        TestWait.Until(() => faults.Count >= 1);
        loop.Stop();

        Assert.Equal("device lost", Assert.Single(faults).Message);
        Assert.Equal(1, source.AcquireCalls);
    }

    [Fact]
    public void StopReturnsPromptlyWhileWaitingForFrames()
    {
        FakeFrameSource source = new();
        FrameLoop loop = Start(source, new FakeMouse(), () => true);

        TestWait.Until(() => source.AcquireCalls >= 3);
        Stopwatch clock = Stopwatch.StartNew();
        loop.Stop();

        Assert.True(clock.Elapsed < TimeSpan.FromSeconds(1), $"Stop took {clock.Elapsed}");
    }

    [Fact]
    public void TheFrameIsRecordedAfterTheMove()
    {
        // The recording must not delay the move.
        ConcurrentQueue<string> events = new();
        FakeFrameSource source = new();
        source.Enqueue(Stopwatch.GetTimestamp(), Centered);
        FakeMouse mouse = new(events);
        FakeRecorder recorder = new(events);
        FrameLoop loop = Start(source, mouse, () => true, recorder);

        TestWait.Until(() => recorder.Recorded >= 1);
        loop.Stop();

        Assert.Equal(["move", "record"], events.Take(2));
    }

    [Fact]
    public void AFrameWithoutATargetIsNotRecorded()
    {
        FakeFrameSource source = new();
        source.Enqueue(Stopwatch.GetTimestamp(), null);
        FakeRecorder recorder = new(new ConcurrentQueue<string>());
        FrameLoop loop = Start(source, new FakeMouse(), () => true, recorder);

        TestWait.Until(() => loop.LoopStatus.Frames >= 1);
        loop.Stop();

        Assert.Equal(0, recorder.Recorded);
    }

    [Fact]
    public void TheRecorderIsDisabledWhileTheKeyIsReleased()
    {
        FakeRecorder recorder = new(new ConcurrentQueue<string>());
        FrameLoop loop = Start(new FakeFrameSource(), new FakeMouse(), () => false, recorder);

        TestWait.Until(() => recorder.Updates >= 3);
        loop.Stop();

        Assert.False(recorder.LastEnabled);
    }

    private static FrameLoop Start(FakeFrameSource source, FakeMouse mouse, Func<bool> isHeld, ISampleRecorder? recorder = null)
    {
        FrameLoop loop = NewLoop(source, mouse, isHeld, Snapshot, recorder: recorder);
        loop.Start();
        return loop;
    }

    private static FrameLoop NewLoop(
        FakeFrameSource source,
        IMouse mouse,
        Func<bool> isHeld,
        Func<AppSettings> settings,
        Action<Exception>? onFault = null,
        ISampleRecorder? recorder = null) =>
        new(new LoopDependencies(
            Frames: source,
            Detector: new FakeDetector(source),
            Mouse: mouse,
            IsHeld: isHeld,
            CurrentSettings: settings,
            Now: Stopwatch.GetTimestamp,
            OnFault: onFault ?? (_ => { }),
            Recorder: recorder));

    private static AppSettings Snapshot() => Snapshot(Smoothing.Auto);

    private static AppSettings Snapshot(Smoothing smoothing) => new() { Mouse = new MouseSettings { Smoothing = smoothing } };

    private sealed class Holder<T>(T value)
        where T : class
    {
        public volatile T Value = value;
    }

    private sealed unsafe class FakeFrameSource : IFrameSource
    {
        private readonly ConcurrentQueue<(long Qpc, Detection? Target)> _frames = new();
        private readonly ConcurrentDictionary<ulong, Detection?> _targets = new();
        private ulong _sequence;
        private int _acquireCalls;
        private int _releaseCount;

        public int InputWidth => 640;

        public int InputHeight => 640;

        public Exception? Fault { get; init; }

        public int AcquireCalls => Volatile.Read(ref _acquireCalls);

        public int ReleaseCount => Volatile.Read(ref _releaseCount);

        public ID3D11Texture2D* DesktopTexture => null;

        public void Enqueue(long presentQpc, Detection? target) => _frames.Enqueue((presentQpc, target));

        public Detection? TargetOf(ulong sequence) => _targets[sequence];

        public void ReleaseHeldFrame() => Interlocked.Increment(ref _releaseCount);

        public CaptureOutcome TryAcquire(int timeoutMs, out CapturedFrame frame)
        {
            Interlocked.Increment(ref _acquireCalls);
            if (Fault is { } fault)
            {
                throw fault;
            }

            if (_frames.TryDequeue(out var queued))
            {
                _sequence++;
                _targets[_sequence] = queued.Target;
                frame = new CapturedFrame(_sequence, queued.Qpc);
                return CaptureOutcome.Ready;
            }

            Thread.Sleep(1);
            frame = default;
            return CaptureOutcome.Timeout;
        }

        public void Dispose()
        {
        }
    }

    private sealed class FakeDetector(FakeFrameSource source) : IDetector
    {
        public int InputWidth => 640;

        public int InputHeight => 640;

        public DetectedFrame Detect(CapturedFrame frame, float threshold) =>
            new(frame.DesktopTimestamp, frame.DesktopTimestamp, frame.DesktopTimestamp, source.TargetOf(frame.Sequence), 0f, default);

        public void Dispose()
        {
        }
    }

    private sealed class FakeMouse(ConcurrentQueue<string>? events = null) : IMouse
    {
        public ConcurrentQueue<(short Dx, short Dy)> Moves { get; } = new();

        public ConcurrentQueue<Smoothing> Smoothings { get; } = new();

        public void Move(short dx, short dy)
        {
            Moves.Enqueue((dx, dy));
            events?.Enqueue("move");
        }

        public void SetSmoothing(Smoothing smoothing) => Smoothings.Enqueue(smoothing);
    }

    private sealed class FakeRecorder(ConcurrentQueue<string> events) : ISampleRecorder
    {
        private int _recorded;
        private int _updates;
        private volatile bool _lastEnabled = true;

        public SampleStatus Status => default;

        public int Recorded => Volatile.Read(ref _recorded);

        public int Updates => Volatile.Read(ref _updates);

        public bool LastEnabled => _lastEnabled;

        public void ApplyEnabled(bool enabled)
        {
            _lastEnabled = enabled;
            Interlocked.Increment(ref _updates);
        }

        public void AdvancePipeline()
        {
        }

        public bool TryRecord(IFrameSource frames)
        {
            events.Enqueue("record");
            Interlocked.Increment(ref _recorded);
            return true;
        }
    }

    /// <summary>Reports the scripted state for each poll. The last state repeats.</summary>
    private sealed class FakeHotkey(params bool[] script)
    {
        private int _polls;

        public int Polls => Volatile.Read(ref _polls);

        public bool IsHeld()
        {
            int poll = Interlocked.Increment(ref _polls) - 1;
            return script[Math.Min(poll, script.Length - 1)];
        }
    }
}
