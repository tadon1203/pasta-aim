using System.Drawing;
using System.Numerics;
using Pstm.Detect;

namespace Pstm.Correction;

public sealed class CorrectionTracker
{
    private const float MinIoU = 0.3f;
    private const float SensitivityDefault = 100f;
    private const float OffsetDefault = 50f;

    private Detection? _last;
    private Vector2 _remainder;

    public CorrectionResult Update(Detection? target, Size input, in CorrectionSettings settings)
    {
        if (!IsSameTarget(target))
        {
            _remainder = Vector2.Zero;
        }

        _last = target;
        if (target is not { } detection)
        {
            return new CorrectionResult(CorrectionOutcome.NoTarget, 0, 0);
        }

        float errorX = detection.CenterX - input.Width * 0.5f;
        float pointY = detection.CenterY + detection.Height * ((settings.Offset - OffsetDefault) / 100f);
        float errorY = pointY - input.Height * 0.5f;

        float nearGain = NearGain(errorX, errorY, settings);
        float gainX = (settings.SensitivityX + nearGain) / SensitivityDefault;
        float gainY = (settings.SensitivityY + nearGain) / SensitivityDefault;
        Vector2 movement = new(errorX * gainX, errorY * gainY);

        if (settings.MaxDelta != 0)
        {
            float limit = settings.MaxDelta;
            float length = MathF.Sqrt(movement.X * movement.X + movement.Y * movement.Y);
            if (length > limit)
            {
                movement *= limit / length;
            }
        }

        Vector2 withRemainder = movement + _remainder;
        short x = ToShort(withRemainder.X);
        short y = ToShort(withRemainder.Y);
        // A saturated axis must not keep its overflow as remainder.
        _remainder = new Vector2(
            Math.Clamp(withRemainder.X - x, -0.5f, 0.5f),
            Math.Clamp(withRemainder.Y - y, -0.5f, 0.5f));

        return new CorrectionResult(x != 0 || y != 0 ? CorrectionOutcome.Move : CorrectionOutcome.OnTarget, x, y);
    }

    /// <summary>
    /// Forgets the target and the remainder. The loop calls it when the keys are released.
    /// </summary>
    public void Reset()
    {
        _last = null;
        _remainder = Vector2.Zero;
    }

    private static float NearGain(float errorX, float errorY, in CorrectionSettings settings)
    {
        if (settings.NearPx <= 0 || settings.NearBoost <= 0)
        {
            return 0f;
        }

        float distance = MathF.Sqrt(errorX * errorX + errorY * errorY);
        float t = Math.Clamp(distance / settings.NearPx, 0f, 1f);
        float near = 1f - t;
        float smooth = near * near * (3f - 2f * near);
        return settings.NearBoost * smooth;
    }

    private bool IsSameTarget(Detection? target) =>
        target is { } current && _last is { } last && Detection.IoU(last, current) >= MinIoU;

    // Round half away from zero. The tracker relies on this rule for symmetric moves.
    private static short ToShort(float value)
    {
        float rounded = MathF.Round(value, MidpointRounding.AwayFromZero);
        return (short)Math.Clamp(rounded, short.MinValue, short.MaxValue);
    }
}
