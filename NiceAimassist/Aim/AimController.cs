using System.Diagnostics;
using System.Drawing;
using System.Numerics;
using NiceAimassist.Detect;

namespace NiceAimassist.Aim;

public sealed class AimController
{
    private const float MinIoU = 0.3f;
    private const float DelaySmoothing = 0.2f;
    private const float MaxLookAhead = 0.2f;
    private const float ExtraLookAhead = 0.01f;
    private const double MaxFrameGap = 0.1;
    private const float SensitivityDefault = 100f;
    private const float TargetYOffsetDefault = 50f;

    private Detection? _last;
    private Vector2 _remainder;
    private Vector2 _velocity;
    private (Vector2 Center, long Desktop)? _previous;
    private float? _delay;

    public AimResult Update(
        Detection? target,
        long desktopTimestamp,
        long nowTimestamp,
        Size input,
        in AimSettings settings)
    {
        UpdateDelayEstimate(desktopTimestamp, nowTimestamp);
        if (!IsSameTarget(target))
        {
            ResetTracking();
        }

        _last = target;
        if (target is not { } detection)
        {
            return new AimResult(AimStatus.NoTarget, 0, 0);
        }

        Vector2 center = new(detection.CenterX, detection.CenterY);
        float gainX = settings.SensitivityX / SensitivityDefault;
        float gainY = settings.SensitivityY / SensitivityDefault;
        float blend = 1f - settings.VelocitySmoothing / 100f;

        Vector2 velocity = UpdateVelocity(center, desktopTimestamp, blend);
        float lookAhead = MathF.Min((_delay ?? 0f) + ExtraLookAhead, MaxLookAhead);
        Vector2 aimed = center + new Vector2(
            Math.Clamp(velocity.X * lookAhead, -detection.Width, detection.Width),
            Math.Clamp(velocity.Y * lookAhead, -detection.Height, detection.Height));

        float errorX = aimed.X - input.Width * 0.5f;
        float aimY = aimed.Y + detection.Height * ((settings.TargetYOffset - TargetYOffsetDefault) / 100f);
        float errorY = aimY - input.Height * 0.5f;
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

        return new AimResult(x != 0 || y != 0 ? AimStatus.Move : AimStatus.OnTarget, x, y);
    }

    public void Reset()
    {
        _last = null;
        _delay = null;
        ResetTracking();
    }

    private void UpdateDelayEstimate(long desktopTimestamp, long nowTimestamp)
    {
        float seconds = (float)(Math.Max(0L, nowTimestamp - desktopTimestamp) / (double)Stopwatch.Frequency);
        _delay = _delay is { } smoothed ? smoothed + DelaySmoothing * (seconds - smoothed) : seconds;
    }

    private bool IsSameTarget(Detection? target) =>
        target is { } current && _last is { } last && Detection.IoU(last, current) >= MinIoU;

    private void ResetTracking()
    {
        _remainder = Vector2.Zero;
        _velocity = Vector2.Zero;
        _previous = null;
    }

    private Vector2 UpdateVelocity(Vector2 center, long desktopTimestamp, float blend)
    {
        if (_previous is { } previous)
        {
            double gap = (desktopTimestamp - previous.Desktop) / (double)Stopwatch.Frequency;
            if (gap > 0 && gap <= MaxFrameGap)
            {
                Vector2 raw = (center - previous.Center) / (float)gap;
                _velocity += blend * (raw - _velocity);
            }
            else
            {
                _velocity = Vector2.Zero;
            }
        }

        _previous = (center, desktopTimestamp);
        return _velocity;
    }

    // Round half away from zero. The aim relies on this rule for symmetric moves.
    private static short ToShort(float value)
    {
        float rounded = MathF.Round(value, MidpointRounding.AwayFromZero);
        return (short)Math.Clamp(rounded, short.MinValue, short.MaxValue);
    }
}
