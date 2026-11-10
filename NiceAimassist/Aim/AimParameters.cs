namespace NiceAimassist.Aim;

public readonly record struct AimParameters(int SensitivityX, int SensitivityY, int TargetYOffset, int MaxDelta, int VelocitySmoothing);
