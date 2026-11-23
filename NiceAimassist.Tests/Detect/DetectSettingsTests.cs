using NiceAimassist.Settings;
using NiceAimassist.Detect;

namespace NiceAimassist.Tests.Detect;

public sealed class DetectSettingsTests
{
    [Fact]
    public void Confidence_is_clamped_into_its_range()
    {
        Assert.Equal(DetectSettings.MaxConfidence, AppSettings.Parse(@"{ ""detect"": { ""confidence"": 150 } }").Detect.Confidence);
        Assert.Equal(0, AppSettings.Parse(@"{ ""detect"": { ""confidence"": -5 } }").Detect.Confidence);
    }

    [Fact]
    public void A_blank_model_name_means_no_model()
    {
        Assert.Null(AppSettings.Parse(@"{ ""detect"": { ""model"": ""  "" } }").Detect.Model);
    }
}
