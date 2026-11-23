using Pstm.Settings;

namespace Pstm.Tests.Settings;

public sealed class SettingsStoreTests : IDisposable
{
    private const int PollMilliseconds = 50;

    private readonly string _directory = Path.Combine(
        Path.GetTempPath(),
        "pstm-settings-" + Guid.NewGuid().ToString("N"));

    private readonly List<SettingsStore> _files = new();

    public SettingsStoreTests()
    {
        Directory.CreateDirectory(_directory);
    }

    public void Dispose()
    {
        foreach (SettingsStore file in _files)
        {
            file.Dispose();
        }

        Directory.Delete(_directory, recursive: true);
    }

    [Fact]
    public void A_missing_file_is_created_with_the_defaults()
    {
        string path = Path.Combine(_directory, "settings.json");
        SettingsStore file = NewFile(path);

        AppSettings loaded = file.Current;

        Assert.Equal(new AppSettings(), loaded);
        Assert.True(File.Exists(path));
        Assert.Equal(new AppSettings(), AppSettings.Parse(File.ReadAllText(path)));
    }

    [Fact]
    public void An_unparsable_file_gives_the_defaults_and_is_not_overwritten()
    {
        string path = Path.Combine(_directory, "settings.json");
        const string broken = "{ \"tracker\": { \"sensitivity_x\": 150";
        File.WriteAllText(path, broken);
        SettingsStore file = NewFile(path);

        AppSettings loaded = file.Current;

        Assert.Equal(new AppSettings(), loaded);
        Assert.Equal(broken, File.ReadAllText(path));
    }

    [Fact]
    public void A_valid_edit_is_published_after_the_poll()
    {
        string path = Path.Combine(_directory, "settings.json");
        SettingsStore file = NewFile(path);

        File.WriteAllText(path, @"{ ""correction"": { ""sensitivity_x"": 300 } }");

        TestWait.Until(() => file.Current.Correction.SensitivityX == 300);
    }

    [Fact]
    public void An_invalid_edit_keeps_the_previous_settings()
    {
        string path = Path.Combine(_directory, "settings.json");
        SettingsStore file = NewFile(path);
        File.WriteAllText(path, @"{ ""correction"": { ""sensitivity_x"": 300 } }");
        TestWait.Until(() => file.Current.Correction.SensitivityX == 300);

        File.WriteAllText(path, "{ \"tracker\": { \"sensitivity_x\": 4");
        Thread.Sleep(PollMilliseconds * 6);
        Assert.Equal(300, file.Current.Correction.SensitivityX);

        File.WriteAllText(path, @"{ ""correction"": { ""sensitivity_x"": 400 } }");
        TestWait.Until(() => file.Current.Correction.SensitivityX == 400);
    }

    [Fact]
    public void A_save_is_current_at_once_and_raises_one_change()
    {
        string path = Path.Combine(_directory, "settings.json");
        SettingsStore file = NewFile(path);
        List<AppSettings> changes = [];
        file.Changed += changes.Add;

        file.Save(file.Current with { Correction = file.Current.Correction with { SensitivityX = 250 } });

        Assert.Equal(250, file.Current.Correction.SensitivityX);
        Assert.Equal(250, AppSettings.Parse(File.ReadAllText(path)).Correction.SensitivityX);

        // The poll reads back the saved text. That read must not raise a second change.
        Thread.Sleep(PollMilliseconds * 6);
        Assert.Single(changes);
    }

    [Fact]
    public void A_save_that_cannot_be_written_stays_current()
    {
        string path = Path.Combine(_directory, "settings.json");
        SettingsStore file = NewFile(path);
        File.Delete(path);
        Directory.CreateDirectory(path);

        file.Save(file.Current with { Correction = file.Current.Correction with { SensitivityX = 250 } });

        Assert.Equal(250, file.Current.Correction.SensitivityX);
    }

    private SettingsStore NewFile(string path)
    {
        SettingsStore file = new(path, PollMilliseconds);
        _files.Add(file);
        return file;
    }
}
