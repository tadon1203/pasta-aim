using System.Collections.Concurrent;
using System.Diagnostics;
using NiceAimassist.Capture;
using NiceAimassist.Dataset;
using NiceAimassist.Detect;
using NiceAimassist.Loop;
using NiceAimassist.Mouse;
using NiceAimassist.Settings;
using Silk.NET.Direct3D11;

namespace NiceAimassist.Tests.Loop;

public class FrameLoopTests
{
    private static readonly TimeSpan WaitLimit = TimeSpan.FromSeconds(5);

    // The target center is 100 px right of the input center (320). The default aim moves dx = 100.
    private static readonly Detection Centered = new(400, 300, 440, 340, 0.9f, 0);

    [Fact]
    public void HeldKeyWithTargetSendsMove()
    {
        FakeSource source = new();
        source.Enqueue(Stopwatch.GetTimestamp(), Centered);
        FakeMouse mouse = new();
        FrameLoop loop = Start(source, mouse, () => true);

        WaitUntil(() => mouse.Moves.Count >= 1);
        loop.Stop();

        (short Dx, short Dy) move = Assert.Single(mouse.Moves);
        Assert.Equal((short)100, move.Dx);
        Assert.Equal((short)0, move.Dy);
    }

    [Fact]
    public void ReleasedKeySendsNoMoveAndDoesNotRunThePipeline()
    {
        FakeSource source = new();
        source.Enqueue(Stopwatch.GetTimestamp(), Centered);
        FakeMouse mouse = new();
        FakeHotkey hotkey = new(false);
        FrameLoop loop = Start(source, mouse, hotkey.IsHeld);

        WaitUntil(() => hotkey.Polls >= 3);
        loop.Stop();

        Assert.True(mouse.Moves.IsEmpty);
        Assert.Equal(0, source.AcquireCalls);
    }

    [Fact]
    public void ReleasingTheKeyResetsTheAimController()
    {
        // Without the reset, the second frame reads the first move as target motion. The aim then overshoots the target.
        long first = Stopwatch.GetTimestamp();
        FakeSource source = new();
        source.Enqueue(first, Centered);
        source.Enqueue(first + Stopwatch.Frequency / 20, Centered);
        FakeMouse mouse = new();
        FakeHotkey hotkey = new(true, false, true);
        FrameLoop loop = Start(source, mouse, hotkey.IsHeld);

        WaitUntil(() => mouse.Moves.Count >= 2);
        loop.Stop();

        (short Dx, short Dy)[] moves = mouse.Moves.ToArray();
        Assert.Equal((short)100, moves[0].Dx);
        Assert.Equal((short)100, moves[1].Dx);
    }

    [Fact]
    public void ReleasingTheKeyEndsTheHeldFrameOnce()
    {
        FakeSource source = new();
        FakeHotkey hotkey = new(true, true, true, false);
        FrameLoop loop = Start(source, new FakeMouse(), hotkey.IsHeld);

        WaitUntil(() => hotkey.Polls >= 8);
        loop.Stop();

        Assert.Equal(1, source.ReleaseCount);
    }

    [Fact]
    public void FrameWithoutTargetSendsNoMove()
    {
        FakeSource source = new();
        source.Enqueue(Stopwatch.GetTimestamp(), null);
        FakeMouse mouse = new();
        FrameLoop loop = Start(source, mouse, () => true);

        WaitUntil(() => loop.LoopStatus.Frames >= 1);
        loop.Stop();

        Assert.True(mouse.Moves.IsEmpty);
    }

    [Fact]
    public void TheMouseGetsTheInterpolationOfTheCurrentSettings()
    {
        // The frames are fed in two stages. The setting change happens between the stages.
        FakeSource source = new();
        FakeMouse mouse = new();
        Holder<AppSettings> settings = new(Snapshot(Interpolation.Percent(30)));
        FrameLoop loop = new(source, source, mouse, () => true, () => settings.Value, Stopwatch.GetTimestamp, _ => { });
        loop.Start();

        source.Enqueue(Stopwatch.GetTimestamp(), null);
        WaitUntil(() => loop.LoopStatus.Frames >= 1);
        Assert.Equal(Interpolation.Percent(30), mouse.Interpolations.Last());
        settings.Value = Snapshot(Interpolation.Auto);
        source.Enqueue(Stopwatch.GetTimestamp(), null);
        WaitUntil(() => loop.LoopStatus.Frames >= 2);
        loop.Stop();

        Assert.Equal(Interpolation.Auto, mouse.Interpolations.Last());
    }

    [Fact]
    public void PipelineExceptionReportsFaultAndEndsTheLoop()
    {
        FakeSource source = new() { Fault = new InvalidOperationException("device lost") };
        ConcurrentQueue<Exception> faults = new();
        FrameLoop loop = new(source, source, new FakeMouse(), () => true, Snapshot, Stopwatch.GetTimestamp, faults.Enqueue);
        loop.Start();

        WaitUntil(() => faults.Count >= 1);
        loop.Stop();

        Assert.Equal("device lost", Assert.Single(faults).Message);
        Assert.Equal(1, source.AcquireCalls);
    }

    [Fact]
    public void StopReturnsPromptlyWhileWaitingForFrames()
    {
        FakeSource source = new();
        FrameLoop loop = Start(source, new FakeMouse(), () => true);

        WaitUntil(() => source.AcquireCalls >= 3);
        Stopwatch clock = Stopwatch.StartNew();
        loop.Stop();

        Assert.True(clock.Elapsed < TimeSpan.FromSeconds(1), $"Stop took {clock.Elapsed}");
    }

    [Fact]
    public void ADueRecorderGetsTheFrameAfterTheMove()
    {
        // The recording must not delay the move.
        ConcurrentQueue<string> events = new();
        FakeSource source = new();
        source.Enqueue(Stopwatch.GetTimestamp(), Centered);
        FakeMouse mouse = new(events);
        FakeRecorder recorder = new(events) { IsDue = true };
        FrameLoop loop = Start(source, mouse, () => true, recorder);

        WaitUntil(() => recorder.Recorded >= 1);
        loop.Stop();

        Assert.Equal(["move", "record"], events.Take(2));
    }

    [Fact]
    public void AFrameWithoutATargetIsNotRecorded()
    {
        FakeSource source = new();
        source.Enqueue(Stopwatch.GetTimestamp(), null);
        FakeRecorder recorder = new(new ConcurrentQueue<string>()) { IsDue = true };
        FrameLoop loop = Start(source, new FakeMouse(), () => true, recorder);

        WaitUntil(() => loop.LoopStatus.Frames >= 1);
        loop.Stop();

        Assert.Equal(0, recorder.Recorded);
    }

    [Fact]
    public void ARecorderThatIsNotDueGetsNoFrame()
    {
        FakeSource source = new();
        source.Enqueue(Stopwatch.GetTimestamp(), Centered);
        FakeRecorder recorder = new(new ConcurrentQueue<string>()) { IsDue = false };
        FrameLoop loop = Start(source, new FakeMouse(), () => true, recorder);

        WaitUntil(() => loop.LoopStatus.Frames >= 1);
        loop.Stop();

        Assert.Equal(0, recorder.Recorded);
    }

    [Fact]
    public void TheRecorderIsUpdatedWhileTheKeyIsReleased()
    {
        FakeRecorder recorder = new(new ConcurrentQueue<string>());
        FrameLoop loop = Start(new FakeSource(), new FakeMouse(), () => false, recorder);

        WaitUntil(() => recorder.Updates >= 3);
        loop.Stop();

        Assert.False(recorder.LastHeld);
    }

    private static FrameLoop Start(FakeSource source, FakeMouse mouse, Func<bool> isHeld, IDatasetControl? recorder = null)
    {
        FrameLoop loop = new(source, source, mouse, isHeld, Snapshot, Stopwatch.GetTimestamp, _ => { }, recorder);
        loop.Start();
        return loop;
    }

    private static AppSettings Snapshot() => Snapshot(Interpolation.Auto);

    private static AppSettings Snapshot(Interpolation interpolation) => new() { Mouse = new MouseSettings { Interpolation = interpolation } };

    private static void WaitUntil(Func<bool> condition)
    {
        long deadline = Stopwatch.GetTimestamp() + (long)(WaitLimit.TotalSeconds * Stopwatch.Frequency);
        while (!condition())
        {
            if (Stopwatch.GetTimestamp() > deadline)
            {
                throw new TimeoutException("the loop did not reach the expected state");
            }

            Thread.Sleep(1);
        }
    }

    private sealed class Holder<T>(T value)
        where T : class
    {
        public volatile T Value = value;
    }

    private sealed unsafe class FakeSource : IFrameSource, IDetector
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

        public void ReleaseHeldFrame() => Interlocked.Increment(ref _releaseCount);

        public CaptureStatus TryAcquire(int timeoutMs, out CapturedFrame frame)
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
                return CaptureStatus.Ready;
            }

            Thread.Sleep(1);
            frame = default;
            return CaptureStatus.Timeout;
        }

        public DetectedFrame Detect(CapturedFrame frame, float confidence) =>
            new(frame.DesktopTimestamp, frame.DesktopTimestamp, frame.DesktopTimestamp, _targets[frame.Sequence], 0f, default);

        public void Dispose()
        {
        }
    }

    private sealed class FakeMouse(ConcurrentQueue<string>? events = null) : IMouse
    {
        public ConcurrentQueue<(short Dx, short Dy)> Moves { get; } = new();

        public ConcurrentQueue<Interpolation> Interpolations { get; } = new();

        public void Move(short dx, short dy)
        {
            Moves.Enqueue((dx, dy));
            events?.Enqueue("move");
        }

        public void SetInterpolation(Interpolation interpolation) => Interpolations.Enqueue(interpolation);
    }

    private sealed class FakeRecorder(ConcurrentQueue<string> events) : IDatasetControl
    {
        private int _recorded;
        private int _updates;
        private volatile bool _lastHeld = true;

        public DatasetStatus Status => default;

        public bool IsDue { get; init; }

        public int Recorded => Volatile.Read(ref _recorded);

        public int Updates => Volatile.Read(ref _updates);

        public bool LastHeld => _lastHeld;

        public void Update(DatasetSettings settings, bool held)
        {
            _lastHeld = held;
            Interlocked.Increment(ref _updates);
        }

        public void Record(IFrameSource frames)
        {
            events.Enqueue("record");
            Interlocked.Increment(ref _recorded);
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
