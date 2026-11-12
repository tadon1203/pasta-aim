using System.Diagnostics;
using System.Drawing;
using NiceAimassist.Aim;
using NiceAimassist.Detect;

namespace NiceAimassist.Tests.Aim;

public class AimControllerTests
{
    private const int InputWidth = 100;
    private const int InputHeight = 100;
    private static readonly Size Input = new(InputWidth, InputHeight);

    private static long Ticks(double seconds) => (long)Math.Round(seconds * Stopwatch.Frequency);

    private static Detection Box(float centerX, float centerY, float width, float height) =>
        new(centerX - width / 2, centerY - height / 2, centerX + width / 2, centerY + height / 2, 0.9f, 0);

    private static Detection At(float centerX, float centerY, float height = 40f) =>
        Box(centerX, centerY, 30f, height);

    private static AimSettings Params(int sensitivity = 100, int targetYOffset = 50, int maxDelta = 0, int smoothing = 0) =>
        new()
        {
            SensitivityX = sensitivity,
            SensitivityY = sensitivity,
            TargetYOffset = targetYOffset,
            MaxDelta = maxDelta,
            VelocitySmoothing = smoothing,
        };

    private static (bool Sent, int X, int Y) Step(AimController aim, Detection? target, double seconds, AimSettings p, double delay = 0)
    {
        long desktop = Ticks(seconds);
        AimResult result = aim.Update(target, desktop, desktop + Ticks(delay), Input, p);
        return (result.Status == AimStatus.Move, result.Dx, result.Dy);
    }

    private static (bool Sent, int X, int Y) Single(Detection target, AimSettings p) =>
        Step(new AimController(), target, 0, p);

    [Fact]
    public void TargetYOffsetAimsInsideTheBoxBeforeSensitivity()
    {
        Assert.Equal((true, 5, 15), Single(At(60, 70), Params(sensitivity: 50, targetYOffset: 75)));
        Assert.Equal((true, 10, 0), Single(At(60, 70), Params(targetYOffset: 0)));
        Assert.Equal((true, 10, 40), Single(At(60, 70), Params(targetYOffset: 100)));
    }

    [Fact]
    public void EachAxisHasItsOwnSensitivity()
    {
        var p = new AimSettings { SensitivityX = 50, SensitivityY = 200, TargetYOffset = 50, MaxDelta = 0, VelocitySmoothing = 0 };
        Assert.Equal((true, 5, 40), Single(At(60, 70), p));
    }

    [Fact]
    public void MaxDeltaCapsTheLengthAndKeepsTheDirection()
    {
        Assert.Equal((true, 4, 9), Single(At(60, 70), Params(maxDelta: 10)));
        Assert.Equal((true, 10, 20), Single(At(60, 70), Params(maxDelta: 100)));
    }

    [Fact]
    public void ZeroMovementSendsNothing()
    {
        Assert.Equal((true, 10, 0), Single(At(60, 50), Params()));
        Assert.Equal((false, 0, 0), Single(At(50, 50), Params()));
    }

    [Fact]
    public void StatusSeparatesNoTargetFromOnTarget()
    {
        var aim = new AimController();
        Assert.Equal(AimStatus.NoTarget, aim.Update(null, 0, 0, Input, Params()).Status);
        Assert.Equal(AimStatus.OnTarget, aim.Update(At(50, 50), 0, 0, Input, Params()).Status);
    }

    [Fact]
    public void TheFrameMovesTowardTheAimPoint()
    {
        Assert.Equal((true, 20, 0), Single(At(70, 50), Params()));
    }

    [Fact]
    public void RemainderAccumulatesSubCountMovement()
    {
        var aim = new AimController();
        var p = Params();
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
        var aim = new AimController();
        var p = Params();
        Assert.False(Step(aim, At(50, 50), 0, p, delay: 0.01).Sent);
        Assert.Equal((true, 20, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.01));
    }

    [Fact]
    public void TheLeadFollowsTheMeasuredDelayBlendedAcrossFrames()
    {
        var aim = new AimController();
        var p = Params();
        Step(aim, At(50, 50), 0, p, delay: 0.010);
        // The blended delay is 0.010 + 0.2 * (0.060 - 0.010) = 0.020. The lead is 0.020.
        Assert.Equal((true, 30, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.060));
    }

    [Fact]
    public void TheLeadIsCappedAtTwoHundredMilliseconds()
    {
        var aim = new AimController();
        var p = Params();
        Step(aim, Box(50, 50, 1000, 40), 0, p, delay: 5);
        Assert.Equal((true, 210, 0), Step(aim, Box(60, 50, 1000, 40), 0.01, p, delay: 5));
    }

    [Fact]
    public void TheLeadIsAtMostOneBoxWide()
    {
        var aim = new AimController();
        var p = Params();
        Step(aim, At(50, 50), 0, p, delay: 0.2);
        Assert.Equal((true, 40, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.2));
    }

    [Fact]
    public void SmoothingBlendsTheNewVelocityIntoTheEstimate()
    {
        var aim = new AimController();
        var p = Params(smoothing: 50);
        Step(aim, At(50, 50), 0, p, delay: 0.01);
        // The measured velocity is 1000 px/s. Half of it is the new estimate. The lead is 0.01 s.
        Assert.Equal((true, 15, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.01));
    }

    [Fact]
    public void TheSentMoveIsAddedBackToTheMeasuredVelocity()
    {
        var aim = new AimController();
        var p = Params();
        // The first frame sends 10 counts at one count per pixel. The target stays at 60 after the move.
        // The own move of 10 px hides 10 px of target motion. The measured velocity is 1000 px/s.
        Assert.Equal((true, 10, 0), Step(aim, At(60, 50), 0, p, delay: 0.01));
        Assert.Equal((true, 20, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.01));
    }

    [Fact]
    public void TheShiftUsesCountsPerPixelAndNotTheSensitivity()
    {
        var aim = new AimController();
        var p = Params(sensitivity: 50) with { CountsPerPixel = 100 };
        // The first move is 5 counts. It shifts the capture by 5 px, not by 10 px.
        // The target stays at 60. The measured velocity is 500 px/s. The lead is 0.02 s, that is 10 px.
        Assert.Equal((true, 5, 0), Step(aim, At(60, 50), 0, p, delay: 0.02));
        Assert.Equal((true, 10, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.02));
    }

    [Fact]
    public void WithoutPredictionTheAimPointStaysOnTheBox()
    {
        var aim = new AimController();
        var p = Params() with { Prediction = false };
        Step(aim, At(50, 50), 0, p, delay: 0.01);
        Assert.Equal((true, 10, 0), Step(aim, At(60, 50), 0.01, p, delay: 0.01));
        Assert.Equal((true, 10, 0), Step(aim, At(60, 50), 0.02, p, delay: 0.01));
    }

    [Fact]
    public void ALongGapASwitchAndAResetGiveNoLead()
    {
        var p = Params();

        var gap = new AimController();
        Step(gap, At(50, 50), 0, p, delay: 0.02);
        Assert.Equal((true, 10, 0), Step(gap, At(60, 50), 1.0, p, delay: 0.02));

        var switched = new AimController();
        Step(switched, At(50, 50), 0, p, delay: 0.02);
        Assert.Equal((true, 45, 0), Step(switched, At(95, 50), 0.01, p, delay: 0.02));

        var reset = new AimController();
        Step(reset, At(50, 50), 0, p, delay: 0.02);
        reset.Reset();
        Assert.Equal((true, 10, 0), Step(reset, At(60, 50), 0.01, p, delay: 0.02));
    }

    [Fact]
    public void ALostTargetClearsTheRemainder()
    {
        var aim = new AimController();
        var p = Params();
        Assert.False(Step(aim, At(50.3f, 50), 0, p).Sent);
        Assert.False(Step(aim, null, 1, p).Sent);
        Assert.False(Step(aim, At(50.3f, 50), 2, p).Sent);
    }

    [Fact]
    public void ResetClearsTheRemainder()
    {
        var aim = new AimController();
        var p = Params();
        Assert.False(Step(aim, At(50.3f, 50), 0, p).Sent);
        aim.Reset();
        Assert.False(Step(aim, At(50.3f, 50), 1, p).Sent);
    }

    [Fact]
    public void AJumpToAnotherBoxClearsTheRemainder()
    {
        var aim = new AimController();
        var p = Params();
        Assert.False(Step(aim, At(50.3f, 50), 0, p).Sent);
        // A carried 0.3 remainder would round 40.3 + 0.3 up to 41. The test expects 40.
        Assert.Equal((true, 40, 0), Step(aim, At(90.3f, 50), 1, p));
    }

    [Fact]
    public void ASaturatedAxisDoesNotKeepAHugeRemainder()
    {
        var aim = new AimController();
        // 400 px at gain 100 is 40000 counts. The value saturates to 32767. The remainder is clamped to 0.5.
        Assert.Equal((true, 32767, 0), Step(aim, At(450, 50), 0, Params(sensitivity: 10000)));
        // 400 px at gain 0.25 is exactly 100. With the clamped 0.5 remainder, it rounds to 101.
        Assert.Equal((true, 101, 0), Step(aim, At(450, 50), 1, Params(sensitivity: 25)));
    }
}
