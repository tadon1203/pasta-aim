using NiceAimassist.Settings;

namespace NiceAimassist.Tests.Settings;

public sealed class SettingsFileTests : IDisposable
{
    private const int DebounceMilliseconds = 50;

    private readonly string _directory = Path.Combine(
        Path.GetTempPath(),
        "nice-aimassist-settings-" + Guid.NewGuid().ToString("N"));

    private readonly List<SettingsFile> _files = new();

    public SettingsFileTests()
    {
        Directory.CreateDirectory(_directory);
    }

    public void Dispose()
    {
        foreach (SettingsFile file in _files)
        {
            file.Dispose();
        }

        Directory.Delete(_directory, recursive: true);
    }

    [Fact]
    public void A_missing_file_is_created_with_the_defaults()
    {
        string path = Path.Combine(_directory, "settings.json");
        SettingsFile file = NewFile(path);

        AppSettings loaded = file.Current;

        Assert.Equal(new AppSettings(), loaded);
        Assert.True(File.Exists(path));
        Assert.Equal(new AppSettings(), AppSettings.Parse(File.ReadAllText(path)));
    }

    [Fact]
    public void An_unparsable_file_gives_the_defaults_and_is_not_overwritten()
    {
        string path = Path.Combine(_directory, "settings.json");
        const string broken = "{ \"aim\": { \"sensitivity_x\": 150";
        File.WriteAllText(path, broken);
        SettingsFile file = NewFile(path);

        AppSettings loaded = file.Current;

        Assert.Equal(new AppSettings(), loaded);
        Assert.Equal(broken, File.ReadAllText(path));
    }

    [Fact]
    public void A_valid_edit_is_published_after_the_debounce()
    {
        string path = Path.Combine(_directory, "settings.json");
        SettingsFile file = NewFile(path);

        File.WriteAllText(path, @"{ ""aim"": { ""sensitivity_x"": 300 } }");

        Assert.True(WaitUntil(() => file.Current.Aim.SensitivityX == 300));
    }

    [Fact]
    public void An_invalid_edit_keeps_the_previous_settings()
    {
        string path = Path.Combine(_directory, "settings.json");
        SettingsFile file = NewFile(path);
        File.WriteAllText(path, @"{ ""aim"": { ""sensitivity_x"": 300 } }");
        Assert.True(WaitUntil(() => file.Current.Aim.SensitivityX == 300));

        File.WriteAllText(path, "{ \"aim\": { \"sensitivity_x\": 4");
        Thread.Sleep(DebounceMilliseconds * 6);
        Assert.Equal(300, file.Current.Aim.SensitivityX);

        File.WriteAllText(path, @"{ ""aim"": { ""sensitivity_x"": 400 } }");
        Assert.True(WaitUntil(() => file.Current.Aim.SensitivityX == 400));
    }

    [Fact]
    public void A_save_is_current_at_once_and_raises_one_change()
    {
        string path = Path.Combine(_directory, "settings.json");
        SettingsFile file = NewFile(path);
        List<AppSettings> changes = [];
        file.Changed += changes.Add;

        file.Save(file.Current with { Aim = file.Current.Aim with { SensitivityX = 250 } });

        Assert.Equal(250, file.Current.Aim.SensitivityX);
        Assert.Equal(250, AppSettings.Parse(File.ReadAllText(path)).Aim.SensitivityX);

        // The watcher reads back the saved file. That read must not raise a second change.
        Thread.Sleep(DebounceMilliseconds * 6);
        Assert.Single(changes);
    }

    [Fact]
    public void A_save_that_cannot_be_written_stays_current()
    {
        string path = Path.Combine(_directory, "settings.json");
        SettingsFile file = NewFile(path);
        File.Delete(path);
        Directory.CreateDirectory(path);

        file.Save(file.Current with { Aim = file.Current.Aim with { SensitivityX = 250 } });

        Assert.Equal(250, file.Current.Aim.SensitivityX);
    }

    private SettingsFile NewFile(string path)
    {
        SettingsFile file = new(path, DebounceMilliseconds);
        _files.Add(file);
        return file;
    }

    private static bool WaitUntil(Func<bool> condition, int timeoutMilliseconds = 5_000)
    {
        DateTime deadline = DateTime.UtcNow.AddMilliseconds(timeoutMilliseconds);
        while (DateTime.UtcNow < deadline)
        {
            if (condition())
            {
                return true;
            }

            Thread.Sleep(10);
        }

        return condition();
    }
}
