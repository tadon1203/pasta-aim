using System.Collections.Concurrent;
using System.Diagnostics;
using NiceAimassist.Settings;
using NiceAimassist.Capture;
using NiceAimassist.Loop;
using NiceAimassist.Mouse;
using NiceAimassist.Detect;

namespace NiceAimassist.Tests.Loop;

public class FrameLoopTests
{
    private static readonly TimeSpan WaitLimit = TimeSpan.FromSeconds(5);

    // The target center is 100 px right of the input center (320). The default aim moves dx = 100.
    private static readonly Detection Centered = new(400, 300, 440, 340, 0.9f, 0);

    [Fact]
    public void HeldKeyWithTargetSendsMove()
    {
        FakeDetector detector = new();
        detector.Enqueue(Stopwatch.GetTimestamp(), Centered);
        FakeMouse mouse = new();
        FrameLoop loop = Start(detector, mouse, () => true);

        WaitUntil(() => mouse.Moves.Count >= 1);
        loop.Stop();

        (short Dx, short Dy) move = Assert.Single(mouse.Moves);
        Assert.Equal((short)100, move.Dx);
        Assert.Equal((short)0, move.Dy);
    }

    [Fact]
    public void ReleasedKeySendsNoMoveAndDoesNotRunThePipeline()
    {
        FakeDetector detector = new();
        detector.Enqueue(Stopwatch.GetTimestamp(), Centered);
        FakeMouse mouse = new();
        FakeHoldKey hold = new(false);
        FrameLoop loop = Start(detector, mouse, hold.IsHeld);

        WaitUntil(() => hold.Polls >= 3);
        loop.Stop();

        Assert.True(mouse.Moves.IsEmpty);
        Assert.Equal(0, detector.RunCalls);
    }

    [Fact]
    public void ReleasingTheKeyResetsTheAimController()
    {
        // Without the reset, the second frame reads the first move as target motion. The aim then overshoots the target.
        long first = Stopwatch.GetTimestamp();
        FakeDetector detector = new();
        detector.Enqueue(first, Centered);
        detector.Enqueue(first + Stopwatch.Frequency / 20, Centered);
        FakeMouse mouse = new();
        FakeHoldKey hold = new(true, false, true);
        FrameLoop loop = Start(detector, mouse, hold.IsHeld);

        WaitUntil(() => mouse.Moves.Count >= 2);
        loop.Stop();

        (short Dx, short Dy)[] moves = mouse.Moves.ToArray();
        Assert.Equal((short)100, moves[0].Dx);
        Assert.Equal((short)100, moves[1].Dx);
    }

    [Fact]
    public void FrameWithoutTargetSendsNoMove()
    {
        FakeDetector detector = new();
        detector.Enqueue(Stopwatch.GetTimestamp(), null);
        FakeMouse mouse = new();
        FrameLoop loop = Start(detector, mouse, () => true);

        WaitUntil(() => loop.LoopStatus.Frames >= 1);
        loop.Stop();

        Assert.True(mouse.Moves.IsEmpty);
    }

    [Fact]
    public void InterpolationIsSentOnceUntilItChanges()
    {
        // The frames are fed in two stages. The setting change happens between the stages.
        FakeDetector detector = new();
        FakeMouse mouse = new();
        Holder<AppSettings> settings = new(Snapshot(Interpolation.Percent(30)));
        FrameLoop loop = new(detector, mouse, () => true, () => settings.Value, Stopwatch.GetTimestamp, _ => { });
        loop.Start();

        detector.Enqueue(Stopwatch.GetTimestamp(), null);
        detector.Enqueue(Stopwatch.GetTimestamp(), null);
        WaitUntil(() => loop.LoopStatus.Frames >= 2);
        settings.Value = Snapshot(Interpolation.Auto);
        detector.Enqueue(Stopwatch.GetTimestamp(), null);
        detector.Enqueue(Stopwatch.GetTimestamp(), null);
        WaitUntil(() => loop.LoopStatus.Frames >= 4);
        loop.Stop();

        Interpolation[] sent = mouse.Interpolations.ToArray();
        Assert.Equal(2, sent.Length);
        Assert.Equal(Interpolation.Percent(30), sent[0]);
        Assert.Equal(Interpolation.Auto, sent[1]);
    }

    [Fact]
    public void PipelineExceptionReportsFaultAndEndsTheLoop()
    {
        FakeDetector detector = new() { Fault = new InvalidOperationException("device lost") };
        ConcurrentQueue<Exception> faults = new();
        FrameLoop loop = new(detector, new FakeMouse(), () => true, Snapshot, Stopwatch.GetTimestamp, faults.Enqueue);
        loop.Start();

        WaitUntil(() => faults.Count >= 1);
        loop.Stop();

        Assert.Equal("device lost", Assert.Single(faults).Message);
        Assert.Equal(1, detector.RunCalls);
    }

    [Fact]
    public void StopReturnsPromptlyWhileWaitingForFrames()
    {
        FakeDetector detector = new();
        FrameLoop loop = Start(detector, new FakeMouse(), () => true);

        WaitUntil(() => detector.RunCalls >= 3);
        Stopwatch clock = Stopwatch.StartNew();
        loop.Stop();

        Assert.True(clock.Elapsed < TimeSpan.FromSeconds(1), $"Stop took {clock.Elapsed}");
    }

    private static FrameLoop Start(FakeDetector detector, FakeMouse mouse, Func<bool> isHeld)
    {
        FrameLoop loop = new(detector, mouse, isHeld, Snapshot, Stopwatch.GetTimestamp, _ => { });
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

    private sealed class Holder<T>(T value) where T : class
    {
        public volatile T Value = value;
    }

    private sealed class FakeDetector : IDetector
    {
        private readonly ConcurrentQueue<(long Qpc, Detection? Target)> _frames = new();
        private int _runCalls;

        public int InputWidth => 640;

        public int InputHeight => 640;

        public Exception? Fault { get; init; }

        public int RunCalls => Volatile.Read(ref _runCalls);

        public void Enqueue(long presentQpc, Detection? target) => _frames.Enqueue((presentQpc, target));

        public CaptureStatus TryDetect(float confidence, out DetectedFrame result)
        {
            Interlocked.Increment(ref _runCalls);
            if (Fault is { } fault)
            {
                throw fault;
            }

            if (_frames.TryDequeue(out var frame))
            {
                result = new DetectedFrame(frame.Qpc, frame.Qpc, frame.Qpc, frame.Target, 0f, default);
                return CaptureStatus.Ready;
            }

            Thread.Sleep(1);
            result = default;
            return CaptureStatus.Timeout;
        }

        public void Dispose()
        {
        }
    }

    private sealed class FakeMouse : IMouse
    {
        public ConcurrentQueue<(short Dx, short Dy)> Moves { get; } = new();

        public ConcurrentQueue<Interpolation> Interpolations { get; } = new();

        public void Move(short dx, short dy) => Moves.Enqueue((dx, dy));

        public void SetInterpolation(Interpolation interpolation) => Interpolations.Enqueue(interpolation);
    }

    /// <summary>Reports the scripted state for each poll. The last state repeats.</summary>
    private sealed class FakeHoldKey(params bool[] script)
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
