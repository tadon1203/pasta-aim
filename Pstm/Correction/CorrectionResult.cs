namespace Pstm.Correction;

public enum CorrectionOutcome
{
    NoTarget,

    /// <summary>There is a target. The rounded movement is (0, 0).</summary>
    OnTarget,

    /// <summary>The rounded movement is not (0, 0). <see cref="CorrectionResult.Dx"/> and <see cref="CorrectionResult.Dy"/> hold the movement.</summary>
    Move,
}

public readonly record struct CorrectionResult(CorrectionOutcome Outcome, short Dx, short Dy);
