using System.Diagnostics;
using System.Numerics;
using NiceAimassist.Pipeline;

namespace NiceAimassist.Aim;

public sealed class AimController
{
    private const float MinOverlap = 0.3f;
    private const float DelayBlend = 0.2f;
    private const float MaxAutoLead = 0.2f;
    private const float LeadExtra = 0.01f;
    private const double MaxFrameGap = 0.1;
    private const float SensitivityDefault = 100f;
    private const float TargetYOffsetDefault = 50f;

    private Detection? _last;
    private Vector2 _carry;
    private Vector2 _velocity;
    private bool _hasPrevious;
    private Vector2 _previousCenter;
    private long _previousCapture;
    private bool _hasDelay;
    private float _delay;

    public bool TryUpdate(
        Detection? target,
        long captureTimestamp,
        long nowTimestamp,
        int inputWidth,
        int inputHeight,
        in AimParameters p,
        out short dx,
        out short dy)
    {
        ObserveDelay(captureTimestamp, nowTimestamp);
        if (!Track(target))
        {
            StartNewTarget();
        }

        dx = 0;
        dy = 0;
        if (target is not { } detection)
        {
            return false;
        }

        float width = detection.X2 - detection.X1;
        float height = detection.Y2 - detection.Y1;
        Vector2 center = new((detection.X1 + detection.X2) * 0.5f, (detection.Y1 + detection.Y2) * 0.5f);
        float gainX = p.SensitivityX / SensitivityDefault;
        float gainY = p.SensitivityY / SensitivityDefault;
        float blend = 1f - p.VelocitySmoothing / 100f;

        Vector2 velocity = Observe(center, captureTimestamp, blend);
        float lead = MathF.Min((_hasDelay ? _delay : 0f) + LeadExtra, MaxAutoLead);
        Vector2 aimed = center + new Vector2(
            Math.Clamp(velocity.X * lead, -MathF.Abs(width), MathF.Abs(width)),
            Math.Clamp(velocity.Y * lead, -MathF.Abs(height), MathF.Abs(height)));

        float errorX = aimed.X - inputWidth * 0.5f;
        float aimY = aimed.Y + height * ((p.TargetYOffset - TargetYOffsetDefault) / 100f);
        float errorY = aimY - inputHeight * 0.5f;
        Vector2 movement = new(errorX * gainX, errorY * gainY);

        if (p.MaxDelta != 0)
        {
            float limit = p.MaxDelta;
            float length = MathF.Sqrt(movement.X * movement.X + movement.Y * movement.Y);
            if (length > limit)
            {
                movement *= limit / length;
            }
        }

        Vector2 weighted = movement + _carry;
        short x = ToShort(weighted.X);
        short y = ToShort(weighted.Y);
        // A saturated axis must not keep its overflow as remainder.
        _carry = new Vector2(
            Math.Clamp(weighted.X - x, -0.5f, 0.5f),
            Math.Clamp(weighted.Y - y, -0.5f, 0.5f));

        dx = x;
        dy = y;
        return x != 0 || y != 0;
    }

    public void Reset()
    {
        _last = null;
        _hasDelay = false;
        StartNewTarget();
    }

    private void ObserveDelay(long captureTimestamp, long nowTimestamp)
    {
        float seconds = (float)(Math.Max(0L, nowTimestamp - captureTimestamp) / (double)Stopwatch.Frequency);
        _delay = _hasDelay ? _delay + DelayBlend * (seconds - _delay) : seconds;
        _hasDelay = true;
    }

    private bool Track(Detection? target)
    {
        bool same = target is { } current && _last is { } last && Overlap(last, current) >= MinOverlap;
        _last = target;
        return same;
    }

    private void StartNewTarget()
    {
        _carry = Vector2.Zero;
        _velocity = Vector2.Zero;
        _hasPrevious = false;
    }

    private Vector2 Observe(Vector2 center, long captureTimestamp, float blend)
    {
        if (_hasPrevious)
        {
            double gap = (captureTimestamp - _previousCapture) / (double)Stopwatch.Frequency;
            if (gap > 0 && gap <= MaxFrameGap)
            {
                Vector2 raw = (center - _previousCenter) / (float)gap;
                _velocity += blend * (raw - _velocity);
            }
            else
            {
                _velocity = Vector2.Zero;
            }
        }

        _hasPrevious = true;
        _previousCenter = center;
        _previousCapture = captureTimestamp;
        return _velocity;
    }

    // Rounds half away from zero, which the aim relies on for symmetric moves.
    private static short ToShort(float value)
    {
        float rounded = MathF.Round(value, MidpointRounding.AwayFromZero);
        return (short)Math.Clamp(rounded, short.MinValue, short.MaxValue);
    }

    private static float Overlap(Detection a, Detection b)
    {
        float width = MathF.Min(a.X2, b.X2) - MathF.Max(a.X1, b.X1);
        float height = MathF.Min(a.Y2, b.Y2) - MathF.Max(a.Y1, b.Y1);
        if (width <= 0 || height <= 0)
        {
            return 0;
        }

        float shared = width * height;
        float together = (a.X2 - a.X1) * (a.Y2 - a.Y1) + (b.X2 - b.X1) * (b.Y2 - b.Y1) - shared;
        return shared / together;
    }
}
