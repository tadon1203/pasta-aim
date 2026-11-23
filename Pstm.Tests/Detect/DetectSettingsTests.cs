using Pstm.Settings;
using Pstm.Detect;

namespace Pstm.Tests.Detect;

public sealed class DetectSettingsTests
{
    [Fact]
    public void Threshold_is_clamped_into_its_range()
    {
        Assert.Equal(DetectSettings.MaxThreshold, AppSettings.Parse(@"{ ""detect"": { ""threshold"": 150 } }").Detect.Threshold);
        Assert.Equal(0, AppSettings.Parse(@"{ ""detect"": { ""threshold"": -5 } }").Detect.Threshold);
    }

    [Fact]
    public void A_blank_model_name_means_no_model()
    {
        Assert.Null(AppSettings.Parse(@"{ ""detect"": { ""model"": ""  "" } }").Detect.Model);
    }
}
