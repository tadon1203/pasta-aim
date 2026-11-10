using System.Collections.Concurrent;
using System.Diagnostics;
using NiceAimassist.Config;
using NiceAimassist.Engine;
using NiceAimassist.Pipeline;
using EngineLoop = NiceAimassist.Engine.Engine;

namespace NiceAimassist.Tests.Engine;

public class EngineTests
{
    private static readonly TimeSpan WaitLimit = TimeSpan.FromSeconds(5);

    // The target center is 100 px right of the input center (320), so the default aim moves dx = 100.
    private static readonly Detection Centered = new(400, 300, 440, 340, 0.9f, 0);

    [Fact]
    public void HeldKeyWithTargetSendsMove()
    {
        FakePipeline pipeline = new();
        pipeline.Enqueue(Stopwatch.GetTimestamp(), Centered);
        FakeMoveSink sink = new();
        EngineLoop engine = Start(pipeline, sink, () => true);

        WaitUntil(() => sink.Moves.Count >= 1);
        engine.Stop();

        (short Dx, short Dy) move = Assert.Single(sink.Moves);
        Assert.Equal((short)100, move.Dx);
        Assert.Equal((short)0, move.Dy);
    }

    [Fact]
    public void ReleasedKeySendsNoMoveAndDoesNotRunThePipeline()
    {
        FakePipeline pipeline = new();
        pipeline.Enqueue(Stopwatch.GetTimestamp(), Centered);
        FakeMoveSink sink = new();
        FakeHoldKey hold = new(false);
        EngineLoop engine = Start(pipeline, sink, hold.IsHeld);

        WaitUntil(() => hold.Polls >= 3);
        engine.Stop();

        Assert.True(sink.Moves.IsEmpty);
        Assert.Equal(0, pipeline.RunCalls);
    }

    [Fact]
    public void ReleasingTheKeyResetsTheAimController()
    {
        // Without the reset, the second frame sees the first move as target motion and aims past the target.
        long first = Stopwatch.GetTimestamp();
        FakePipeline pipeline = new();
        pipeline.Enqueue(first, Centered);
        pipeline.Enqueue(first + Stopwatch.Frequency / 20, Centered);
        FakeMoveSink sink = new();
        FakeHoldKey hold = new(true, false, true);
        EngineLoop engine = Start(pipeline, sink, hold.IsHeld);

        WaitUntil(() => sink.Moves.Count >= 2);
        engine.Stop();

        (short Dx, short Dy)[] moves = sink.Moves.ToArray();
        Assert.Equal((short)100, moves[0].Dx);
        Assert.Equal((short)100, moves[1].Dx);
    }

    [Fact]
    public void FrameWithoutTargetSendsNoMove()
    {
        FakePipeline pipeline = new();
        pipeline.Enqueue(Stopwatch.GetTimestamp(), null);
        FakeMoveSink sink = new();
        EngineLoop engine = Start(pipeline, sink, () => true);

        WaitUntil(() => engine.Stats.Frames >= 1);
        engine.Stop();

        Assert.True(sink.Moves.IsEmpty);
    }

    [Fact]
    public void InterpolationIsSentOnceUntilItChanges()
    {
        // Frames are fed in two stages so that the setting change lands between them.
        FakePipeline pipeline = new();
        FakeMoveSink sink = new();
        Holder<Settings> settings = new(Snapshot(Interpolation.Percent(30)));
        EngineLoop engine = new(pipeline, sink, () => true, () => settings.Value, Stopwatch.GetTimestamp, _ => { });
        engine.Start();

        pipeline.Enqueue(Stopwatch.GetTimestamp(), null);
        pipeline.Enqueue(Stopwatch.GetTimestamp(), null);
        WaitUntil(() => engine.Stats.Frames >= 2);
        settings.Value = Snapshot(Interpolation.Auto);
        pipeline.Enqueue(Stopwatch.GetTimestamp(), null);
        pipeline.Enqueue(Stopwatch.GetTimestamp(), null);
        WaitUntil(() => engine.Stats.Frames >= 4);
        engine.Stop();

        Interpolation[] sent = sink.Interpolations.ToArray();
        Assert.Equal(2, sent.Length);
        Assert.Equal(Interpolation.Percent(30), sent[0]);
        Assert.Equal(Interpolation.Auto, sent[1]);
    }

    [Fact]
    public void PipelineExceptionReportsFaultAndEndsTheLoop()
    {
        FakePipeline pipeline = new() { Fault = new InvalidOperationException("device lost") };
        ConcurrentQueue<Exception> faults = new();
        EngineLoop engine = new(pipeline, new FakeMoveSink(), () => true, Snapshot, Stopwatch.GetTimestamp, faults.Enqueue);
        engine.Start();

        WaitUntil(() => faults.Count >= 1);
        engine.Stop();

        Assert.Equal("device lost", Assert.Single(faults).Message);
        Assert.Equal(1, pipeline.RunCalls);
    }

    [Fact]
    public void StopReturnsPromptlyWhileWaitingForFrames()
    {
        FakePipeline pipeline = new();
        EngineLoop engine = Start(pipeline, new FakeMoveSink(), () => true);

        WaitUntil(() => pipeline.RunCalls >= 3);
        Stopwatch clock = Stopwatch.StartNew();
        engine.Stop();

        Assert.True(clock.Elapsed < TimeSpan.FromSeconds(1), $"Stop took {clock.Elapsed}");
    }

    private static EngineLoop Start(FakePipeline pipeline, FakeMoveSink sink, Func<bool> isHeld)
    {
        EngineLoop engine = new(pipeline, sink, isHeld, Snapshot, Stopwatch.GetTimestamp, _ => { });
        engine.Start();
        return engine;
    }

    private static Settings Snapshot() => Snapshot(Interpolation.Auto);

    private static Settings Snapshot(Interpolation interpolation) => new() { Interpolation = interpolation };

    private static void WaitUntil(Func<bool> condition)
    {
        long deadline = Stopwatch.GetTimestamp() + (long)(WaitLimit.TotalSeconds * Stopwatch.Frequency);
        while (!condition())
        {
            if (Stopwatch.GetTimestamp() > deadline)
            {
                throw new TimeoutException("the engine did not reach the expected state");
            }

            Thread.Sleep(1);
        }
    }

    private sealed class Holder<T>(T value) where T : class
    {
        public volatile T Value = value;
    }

    private sealed class FakePipeline : IFramePipeline
    {
        private readonly ConcurrentQueue<(long Qpc, Detection? Target)> _frames = new();
        private int _runCalls;

        public int InputWidth => 640;

        public int InputHeight => 640;

        public Exception? Fault { get; init; }

        public int RunCalls => Volatile.Read(ref _runCalls);

        public void Enqueue(long presentQpc, Detection? target) => _frames.Enqueue((presentQpc, target));

        public bool TryRun(int timeoutMs, float confidence, out long presentQpc, out Detection? target)
        {
            Interlocked.Increment(ref _runCalls);
            if (Fault is { } fault)
            {
                throw fault;
            }

            if (_frames.TryDequeue(out var frame))
            {
                presentQpc = frame.Qpc;
                target = frame.Target;
                return true;
            }

            Thread.Sleep(1);
            presentQpc = 0;
            target = null;
            return false;
        }

        public void Dispose()
        {
        }
    }

    private sealed class FakeMoveSink : IMoveSink
    {
        public ConcurrentQueue<(short Dx, short Dy)> Moves { get; } = new();

        public ConcurrentQueue<Interpolation> Interpolations { get; } = new();

        public void Move(short dx, short dy) => Moves.Enqueue((dx, dy));

        public void SetInterpolation(Interpolation interpolation) => Interpolations.Enqueue(interpolation);
    }

    /// <summary>Reports the scripted state for each poll; the last state repeats.</summary>
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
