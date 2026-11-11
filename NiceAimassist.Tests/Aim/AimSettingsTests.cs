using NiceAimassist.Config;

namespace NiceAimassist.Tests.Aim;

public sealed class AimSettingsTests
{
    [Fact]
    public void A_missing_aim_section_takes_the_defaults()
    {
        var aim = Settings.Parse("{}").Aim;

        Assert.Equal((100, 100, 50, 512, 50), (aim.SensitivityX, aim.SensitivityY, aim.TargetYOffset, aim.MaxDelta, aim.VelocitySmoothing));
    }

    [Fact]
    public void Out_of_range_values_are_clamped_into_their_range()
    {
        var aim = Settings.Parse(
            @"{ ""aim"": { ""sensitivity_x"": 0, ""sensitivity_y"": 20000, ""target_y_offset"": -1,
                ""max_delta"": -5, ""velocity_smoothing"": 200 } }").Aim;

        Assert.Equal((1, 10_000, 0, 0, 95), (aim.SensitivityX, aim.SensitivityY, aim.TargetYOffset, aim.MaxDelta, aim.VelocitySmoothing));
    }
}
