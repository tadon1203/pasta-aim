using NiceAimassist.Settings;

namespace NiceAimassist.Tests.Aim;

public sealed class AimSettingsTests
{
    [Fact]
    public void A_missing_aim_section_takes_the_defaults()
    {
        var aim = AppSettings.Parse("{}").Aim;

        Assert.Equal((100, 100, 50, 512, 12), (aim.SensitivityX, aim.SensitivityY, aim.TargetYOffset, aim.MaxDelta, aim.VelocitySmoothingMs));
    }

    [Fact]
    public void Out_of_range_values_are_clamped_into_their_range()
    {
        var aim = AppSettings.Parse(
            @"{ ""aim"": { ""sensitivity_x"": 0, ""sensitivity_y"": 20000, ""target_y_offset"": -1,
                ""max_delta"": -5, ""velocity_smoothing_ms"": 5000 } }").Aim;

        Assert.Equal((1, 10_000, 0, 0, 1000), (aim.SensitivityX, aim.SensitivityY, aim.TargetYOffset, aim.MaxDelta, aim.VelocitySmoothingMs));
    }
}
