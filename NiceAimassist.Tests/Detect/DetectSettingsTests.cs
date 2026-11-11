using NiceAimassist.Config;
using NiceAimassist.Detect;

namespace NiceAimassist.Tests.Detect;

public sealed class DetectSettingsTests
{
    [Fact]
    public void Confidence_defaults_to_fifty_and_is_clamped_into_its_range()
    {
        Assert.Equal(50, Settings.Parse("{}").Detect.Confidence);
        Assert.Equal(DetectSettings.MaxConfidence, Settings.Parse(@"{ ""detect"": { ""confidence"": 150 } }").Detect.Confidence);
        Assert.Equal(0, Settings.Parse(@"{ ""detect"": { ""confidence"": -5 } }").Detect.Confidence);
    }

    [Fact]
    public void A_blank_model_name_means_no_model()
    {
        Assert.Null(Settings.Parse(@"{ ""detect"": { ""model"": ""  "" } }").Detect.Model);
    }
}
