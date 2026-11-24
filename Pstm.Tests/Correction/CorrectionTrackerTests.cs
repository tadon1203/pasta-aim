using System.Drawing;
using Pstm.Correction;
using Pstm.Detect;

namespace Pstm.Tests.Correction;

public class CorrectionTrackerTests
{
    private const int InputWidth = 100;
    private const int InputHeight = 100;
    private static readonly Size Input = new(InputWidth, InputHeight);

    private static Detection Box(float centerX, float centerY, float width, float height) =>
        new(centerX - width / 2, centerY - height / 2, centerX + width / 2, centerY + height / 2, 0.9f, 0);

    private static Detection At(float centerX, float centerY, float height = 40f) =>
        Box(centerX, centerY, 30f, height);

    // No cap on the length. The near boost is off so the base gain is easy to check.
    private static CorrectionSettings NoCap(int sensitivity = 100, int targetYOffset = 50, int maxDelta = 0) =>
        new()
        {
            SensitivityX = sensitivity,
            SensitivityY = sensitivity,
            Offset = targetYOffset,
            MaxDelta = maxDelta,
            NearBoost = 0,
        };

    private static (bool Sent, int X, int Y) Step(CorrectionTracker tracker, Detection? target, CorrectionSettings p)
    {
        CorrectionResult result = tracker.Update(target, Input, p);
        return (result.Outcome == CorrectionOutcome.Move, result.Dx, result.Dy);
    }

    private static (bool Sent, int X, int Y) Single(Detection target, CorrectionSettings p) =>
        Step(new CorrectionTracker(), target, p);

    [Fact]
    public void OffsetCorrectionsInsideTheBoxBeforeSensitivity()
    {
        Assert.Equal((true, 5, 15), Single(At(60, 70), NoCap(sensitivity: 50, targetYOffset: 75)));
        Assert.Equal((true, 10, 0), Single(At(60, 70), NoCap(targetYOffset: 0)));
        Assert.Equal((true, 10, 40), Single(At(60, 70), NoCap(targetYOffset: 100)));
    }

    [Fact]
    public void EachAxisHasItsOwnSensitivity()
    {
        var p = new CorrectionSettings { SensitivityX = 50, SensitivityY = 200, Offset = 50, MaxDelta = 0, NearBoost = 0 };
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
        var tracker = new CorrectionTracker();
        Assert.Equal(CorrectionOutcome.NoTarget, tracker.Update(null, Input, NoCap()).Outcome);
        Assert.Equal(CorrectionOutcome.OnTarget, tracker.Update(At(50, 50), Input, NoCap()).Outcome);
    }

    [Fact]
    public void RemainderAccumulatesSubCountMovement()
    {
        var tracker = new CorrectionTracker();
        var p = NoCap();
        var sent = new (bool, int, int)[4];
        for (int i = 0; i < sent.Length; i++)
        {
            sent[i] = Step(tracker, At(50.3f, 49.7f), p);
        }

        Assert.Equal(new (bool, int, int)[] { (false, 0, 0), (true, 1, -1), (false, 0, 0), (false, 0, 0) }, sent);
    }

    [Fact]
    public void TheGainIsTheBaseOutsideTheNearRadius()
    {
        var p = NoCap(sensitivity: 50) with { NearPx = 20, NearBoost = 30 };
        Assert.Equal((true, 15, 0), Single(At(80, 50), p));
    }

    [Fact]
    public void TheNearBoostAddsGainInsideTheRadius()
    {
        var p = NoCap(sensitivity: 50) with { NearPx = 20, NearBoost = 30 };
        // The distance is 10, half of the radius. The smooth curve is 0.5, so the gain is 50 + 15 = 65.
        Assert.Equal((true, 7, 0), Single(At(60, 50), p));
    }

    [Fact]
    public void TheNearBoostIsZeroAtTheRadius()
    {
        var p = NoCap(sensitivity: 50) with { NearPx = 20, NearBoost = 30 };
        Assert.Equal((true, 10, 0), Single(At(70, 50), p));
    }

    [Fact]
    public void TheNearBoostAddsTheSameGainToBothAxes()
    {
        var p = NoCap(sensitivity: 50) with { NearPx = 20, NearBoost = 30 };
        Assert.Equal((true, 6, 6), Single(At(60, 60), p));
    }

    [Fact]
    public void TheNearBoostNeverExceedsTheMaxStep()
    {
        var p = NoCap(sensitivity: 1000, maxDelta: 10) with { NearPx = 20, NearBoost = 100 };
        Assert.Equal((true, 10, 0), Single(At(60, 50), p));
    }

    [Fact]
    public void ZeroNearBoostKeepsTheBaseGain()
    {
        var p = NoCap(sensitivity: 50) with { NearPx = 20, NearBoost = 0 };
        Assert.Equal((true, 5, 0), Single(At(60, 50), p));
    }

    [Fact]
    public void ZeroNearPxDisablesTheBoost()
    {
        var p = NoCap(sensitivity: 50) with { NearPx = 0, NearBoost = 30 };
        Assert.Equal((true, 5, 0), Single(At(60, 50), p));
    }

    [Fact]
    public void ALostTargetClearsTheRemainder()
    {
        var tracker = new CorrectionTracker();
        var p = NoCap();
        Assert.False(Step(tracker, At(50.3f, 50), p).Sent);
        Assert.False(Step(tracker, null, p).Sent);
        Assert.False(Step(tracker, At(50.3f, 50), p).Sent);
    }

    [Fact]
    public void ResetClearsTheRemainder()
    {
        var tracker = new CorrectionTracker();
        var p = NoCap();
        Assert.False(Step(tracker, At(50.3f, 50), p).Sent);
        tracker.Reset();
        Assert.False(Step(tracker, At(50.3f, 50), p).Sent);
    }

    [Fact]
    public void AJumpToAnotherBoxClearsTheRemainder()
    {
        var tracker = new CorrectionTracker();
        var p = NoCap();
        Assert.False(Step(tracker, At(50.3f, 50), p).Sent);
        // A carried 0.3 remainder would round 40.3 + 0.3 up to 41. The test expects 40.
        Assert.Equal((true, 40, 0), Step(tracker, At(90.3f, 50), p));
    }

    [Fact]
    public void ASaturatedAxisDoesNotKeepAHugeRemainder()
    {
        var tracker = new CorrectionTracker();
        // 400 px at gain 100 is 40000 counts. The value saturates to 32767. The remainder is clamped to 0.5.
        Assert.Equal((true, 32767, 0), Step(tracker, At(450, 50), NoCap(sensitivity: 10000)));
        // 400 px at gain 0.25 is exactly 100. With the clamped 0.5 remainder, it rounds to 101.
        Assert.Equal((true, 101, 0), Step(tracker, At(450, 50), NoCap(sensitivity: 25)));
    }
}
