using NiceAimassist.Settings;
using NiceAimassist.Shared.Logging;

namespace NiceAimassist.Tests.Shared.Logging;

public sealed class LogSettingsTests
{
    [Theory]
    [InlineData(@"{}", LogLevel.Info)]
    [InlineData(@"{ ""log"": { ""level"": ""trace"" } }", LogLevel.Trace)]
    [InlineData(@"{ ""log"": { ""level"": ""Warn"" } }", LogLevel.Warn)]
    [InlineData(@"{ ""log"": { ""level"": 9 } }", LogLevel.Info)]
    public void Level_reads_a_name_and_falls_back_to_info_for_an_unknown_number(string json, LogLevel expected)
    {
        Assert.Equal(expected, AppSettings.Parse(json).Log.Level);
    }

    [Fact]
    public void Level_serializes_as_a_name_and_reads_back_equal()
    {
        AppSettings settings = new() { Log = new LogSettings { Level = LogLevel.Debug } };

        Assert.Contains("\"level\": \"Debug\"", settings.ToJson());
        Assert.Equal(settings, AppSettings.Parse(settings.ToJson()));
    }
}
