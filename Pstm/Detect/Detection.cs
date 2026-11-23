namespace Pstm.Detect;

/// <summary>One model output row. The coordinates are model-input pixels. The origin is the top left. The decoder keeps only boxes with X2 &gt; X1 and Y2 &gt; Y1.</summary>
public readonly record struct Detection(float X1, float Y1, float X2, float Y2, float Score, int Class)
{
    public float Width => X2 - X1;

    public float Height => Y2 - Y1;

    public float CenterX => (X1 + X2) * 0.5f;

    public float CenterY => (Y1 + Y2) * 0.5f;

    public float Area => Width * Height;

    /// <summary>Intersection over union of two boxes, in 0..1. Disjoint boxes give 0.</summary>
    public static float IoU(Detection a, Detection b)
    {
        float width = MathF.Min(a.X2, b.X2) - MathF.Max(a.X1, b.X1);
        float height = MathF.Min(a.Y2, b.Y2) - MathF.Max(a.Y1, b.Y1);
        if (width <= 0 || height <= 0)
        {
            return 0;
        }

        float shared = width * height;
        return shared / (a.Area + b.Area - shared);
    }
}
