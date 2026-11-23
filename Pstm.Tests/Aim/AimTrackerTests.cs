using System.Diagnostics;
using System.Drawing;
using Pstm.Aim;
using Pstm.Detect;

namespace Pstm.Tests.Aim;

public class AimTrackerTests
{
    private const int InputWidth = 100;
    private const int InputHeight = 100;
    private static readonly Size Input = new(InputWidth, InputHeight);

    private static long Ticks(double seconds) => (long)Math.Round(seconds * Stopwatch.Frequency);

    private static Detection Box(float centerX, float centerY, float width, float height) =>
        new(centerX - width / 2, centerY - height / 2, centerX + width / 2, centerY + height / 2, 0.9f, 0);

    private static Detection At(float centerX, float centerY, float height = 40f) =>
        Box(centerX, centerY, 30f, height);

    // No cap on the length. The production default of MaxDelta is 512.
    private static AimSettings NoCap(int sensitivity = 100, int targetYOffset = 50, int maxDelta = 0, int smoothing = 0) =>
        new()
        {
            SensitivityX = sensitivity,
            SensitivityY = sensitivity,
            TargetYOffset = targetYOffset,
            MaxDelta = maxDelta,
            VelocitySmoothingMs = smoothing,
        };

    private static (bool Sent, int X, int Y) Step(AimTracker aim, Detection? target, double seconds, AimSettings p, double delay = 0)
    {
        long desktop = Ticks(seconds);
        AimResult result = aim.Update(target, desktop, desktop + Ticks(delay), Input, p);
        return (result.Outcome == AimOutcome.Move, result.Dx, result.Dy);
    }

    private static (bool Sent, int X, int Y) Single(Detection target, AimSettings p) =>
        Step(new AimTracker(), target, 0, p);

    [Fact]
    public void TargetYOffsetAimsInsideTheBoxBeforeSensitivity()
    {
        Assert.Equal((true, 5, 15), Single(At(60, 70), NoCap(sensitivity: 50, targetYOffset: 75)));
        Assert.Equal((true, 10, 0), Single(At(60, 70), NoCap(targetYOffset: 0)));
        Assert.Equal((true, 10, 40), Single(At(60, 70), NoCap(targetYOffset: 100)));
    }

    [Fact]
    public void EachAxisHasItsOwnSensitivity()
    {
        var p = new AimSettings { SensitivityX = 50, SensitivityY = 200, TargetYOffset = 50, MaxDelta = 0, VelocitySmoothingMs = 0 };
        Assert.Equal((true, 5, 40), Single(At(60, 70), p));
    }

    [Fact]
    public void MaxDeltaCapsTheLengthAndKeepsTheDirection()
    {
        Assert.Equal((true, 4, 9), Single(At(60, 70), NoCap(maxDelta: 10)));
        Assert.Equal((true, 10, 20), Single(At(60, 70), NoCap(maxDelta: 100)));
    }

    [Fact]
    public void ZeroMovementSendsNothing()
    {
        Assert.Equal((true, 10, 0), Single(At(60, 50), NoCap()));
        Assert.Equal((false, 0, 0), Single(At(50, 50), NoCap()));
    }

    [Fact]
    public void StatusSeparatesNoTargetFromOnTarget()
    {
        var aim = new AimTracker();
        Assert.Equal(AimOutcome.NoTarget, aim.Update(null, 0, 0, Input, NoCap()).Outcome);
        Assert.Equal(AimOutcome.OnTarget, aim.Update(At(50, 50), 0, 0, Input, NoCap()).Outcome);
    }

    [Fact]
    public void RemainderAccumulatesSubCountMovement()
    {
        var aim = new AimTracker();
        var p = NoCap();
        // The frames are one second apart. The velocity estimate stays at zero.
        var sent = new (bool, int, int)[4];
        for (int i = 0; i < sent.Length; i++)
        {
            sent[i] = Step(aim, At(50.3f, 49.7f), i, p);
        }

        Assert.Equal(new (bool, int, int)[] { (false, 0, 0), (true, 1, -1), (false, 0, 0), (false, 0, 0) }, sent);
    }

    [Fact]
    public void TheAimPointLeadsAMovingTargetByItsVelocity()
    {
        var aim = new AimTracker();
        var p = NoCap();
        Assert.False(Step(aim, At(50, 50), 0, p, delay: 0.01).Sent);
        Assert.Equal((true, 20, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.01));
    }

    [Fact]
    public void TheLeadFollowsTheMeasuredDelayBlendedAcrossFrames()
    {
        var aim = new AimTracker();
        var p = NoCap();
        Step(aim, At(50, 50), 0, p, delay: 0.010);
        // The blended delay is 0.010 + 0.2 * (0.060 - 0.010) = 0.020. The lead is 0.020.
        Assert.Equal((true, 30, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.060));
    }

    [Fact]
    public void TheLeadIsCappedAtTwoHundredMilliseconds()
    {
        var aim = new AimTracker();
        var p = NoCap();
        Step(aim, Box(50, 50, 1000, 40), 0, p, delay: 5);
        Assert.Equal((true, 210, 0), Step(aim, Box(60, 50, 1000, 40), 0.01, p, delay: 5));
    }

    [Fact]
    public void TheLeadIsAtMostOneBoxWide()
    {
        var aim = new AimTracker();
        var p = NoCap();
        Step(aim, At(50, 50), 0, p, delay: 0.2);
        Assert.Equal((true, 40, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.2));
    }

    [Fact]
    public void SmoothingBlendsTheNewVelocityIntoTheEstimate()
    {
        var aim = new AimTracker();
        var p = NoCap(smoothing: 10);
        Step(aim, At(50, 50), 0, p, delay: 0.01);
        // The measured velocity is 1000 px/s. The gap equals the smoothing time, so the blend is 1 - 1/e = 0.632.
        // The lead is 0.01 s, that is 6.32 px.
        Assert.Equal((true, 16, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.01));
    }

    [Fact]
    public void TheSmoothingTimeDoesNotDependOnTheFrameRate()
    {
        // The own move shifts the next capture. The low sensitivity and the high counts per pixel make that shift zero.
        var p = NoCap(sensitivity: 1, smoothing: 20) with { CountsPerPixel = 10_000 };
        var last = p with { SensitivityX = 10_000, SensitivityY = 10_000 };
        var slow = new AimTracker();
        var fast = new AimTracker();
        Step(slow, At(50, 50), 0, p, delay: 0.01);
        Step(fast, At(50, 50), 0, p, delay: 0.01);
        Step(fast, At(55, 50), 0.005, p, delay: 0.01);
        Step(fast, At(60, 50), 0.01, p, delay: 0.01);
        Step(fast, At(65, 50), 0.015, p, delay: 0.01);
        Step(slow, At(60, 50), 0.01, p, delay: 0.01);
        var expected = Step(slow, At(70, 50), 0.02, last, delay: 0.01);
        var actual = Step(fast, At(70, 50), 0.02, last, delay: 0.01);
        Assert.True(expected.Sent);
        Assert.InRange(actual.X, expected.X - 5, expected.X + 5);
    }

    [Fact]
    public void TheSentMoveIsAddedBackToTheMeasuredVelocity()
    {
        var aim = new AimTracker();
        var p = NoCap();
        // The first frame sends 10 counts at one count per pixel. The target stays at 60 after the move.
        // The own move of 10 px hides 10 px of target motion. The measured velocity is 1000 px/s.
        Assert.Equal((true, 10, 0), Step(aim, At(60, 50), 0, p, delay: 0.01));
        Assert.Equal((true, 20, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.01));
    }

    [Fact]
    public void TheShiftUsesCountsPerPixelAndNotTheSensitivity()
    {
        var aim = new AimTracker();
        var p = NoCap(sensitivity: 50) with { CountsPerPixel = 100 };
        // The first move is 5 counts. It shifts the capture by 5 px, not by 10 px.
        // The target stays at 60. The measured velocity is 500 px/s. The lead is 0.02 s, that is 10 px.
        Assert.Equal((true, 5, 0), Step(aim, At(60, 50), 0, p, delay: 0.02));
        Assert.Equal((true, 10, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.02));
    }

    [Fact]
    public void WithoutPredictionTheAimPointStaysOnTheBox()
    {
        var aim = new AimTracker();
        var p = NoCap() with { Prediction = false };
        Step(aim, At(50, 50), 0, p, delay: 0.01);
        Assert.Equal((true, 10, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.01));
        Assert.Equal((true, 10, 0), Step(aim, At(60, 50), 0.02, p, delay: 0.01));
    }

    [Fact]
    public void ALongGapASwitchAndAResetGiveNoLead()
    {
        var p = NoCap();

        var gap = new AimTracker();
        Step(gap, At(50, 50), 0, p, delay: 0.02);
        Assert.Equal((true, 10, 0), Step(gap, At(60, 50), 1.0, p, delay: 0.02));

        var switched = new AimTracker();
        Step(switched, At(50, 50), 0, p, delay: 0.02);
        Assert.Equal((true, 45, 0), Step(switched, At(95, 50), 0.01, p, delay: 0.02));

        var reset = new AimTracker();
        Step(reset, At(50, 50), 0, p, delay: 0.02);
        reset.Reset();
        Assert.Equal((true, 10, 0), Step(reset, At(60, 50), 0.01, p, delay: 0.02));
    }

    [Fact]
    public void ALostTargetClearsTheRemainder()
    {
        var aim = new AimTracker();
        var p = NoCap();
        Assert.False(Step(aim, At(50.3f, 50), 0, p).Sent);
        Assert.False(Step(aim, null, 1, p).Sent);
        Assert.False(Step(aim, At(50.3f, 50), 2, p).Sent);
    }

    [Fact]
    public void ResetClearsTheRemainder()
    {
        var aim = new AimTracker();
        var p = NoCap();
        Assert.False(Step(aim, At(50.3f, 50), 0, p).Sent);
        aim.Reset();
        Assert.False(Step(aim, At(50.3f, 50), 1, p).Sent);
    }

    [Fact]
    public void AJumpToAnotherBoxClearsTheRemainder()
    {
        var aim = new AimTracker();
        var p = NoCap();
        Assert.False(Step(aim, At(50.3f, 50), 0, p).Sent);
        // A carried 0.3 remainder would round 40.3 + 0.3 up to 41. The test expects 40.
        Assert.Equal((true, 40, 0), Step(aim, At(90.3f, 50), 1, p));
    }

    [Fact]
    public void ASaturatedAxisDoesNotKeepAHugeRemainder()
    {
        var aim = new AimTracker();
        // 400 px at gain 100 is 40000 counts. The value saturates to 32767. The remainder is clamped to 0.5.
        Assert.Equal((true, 32767, 0), Step(aim, At(450, 50), 0, NoCap(sensitivity: 10000)));
        // 400 px at gain 0.25 is exactly 100. With the clamped 0.5 remainder, it rounds to 101.
        Assert.Equal((true, 101, 0), Step(aim, At(450, 50), 1, NoCap(sensitivity: 25)));
    }
}
