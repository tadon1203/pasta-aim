namespace NiceAimassist.Aim;

public enum AimOutcome
{
    /// <summary>There is no target.</summary>
    NoTarget,

    /// <summary>There is a target, but the rounded movement is (0, 0).</summary>
    OnTarget,

    /// <summary>The rounded movement is not (0, 0); <see cref="AimResult.Dx"/> and <see cref="AimResult.Dy"/> hold it.</summary>
    Move,
}

public readonly record struct AimResult(AimOutcome Outcome, short Dx, short Dy);
