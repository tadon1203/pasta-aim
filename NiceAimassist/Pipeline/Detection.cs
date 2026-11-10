namespace NiceAimassist.Pipeline;

/// <summary>One model output row, in model-input pixel coordinates (origin top-left).</summary>
public readonly record struct Detection(float X1, float Y1, float X2, float Y2, float Score, int Class);
