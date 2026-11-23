using Pstm.Settings;

namespace Pstm.Tests.Correction;

public sealed class CorrectionSettingsTests
{
    [Fact]
    public void Out_of_range_values_are_clamped_into_their_range()
    {
        var tracker = AppSettings.Parse(
            @"{ ""correction"": { ""sensitivity_x"": 0, ""sensitivity_y"": 20000, ""offset"": -1,
                ""max_delta"": -5, ""lead_smoothing_ms"": 5000 } }").Correction;

        Assert.Equal((1, 10_000, 0, 0, 1000), (tracker.SensitivityX, tracker.SensitivityY, tracker.Offset, tracker.MaxDelta, tracker.LeadSmoothingMs));
    }
}
