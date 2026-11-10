
using NiceAimassist.Config;

namespace NiceAimassist.Tests.Config;

public sealed class SettingsStoreTests : IDisposable
{
    private const int DebounceMilliseconds = 50;

    private readonly string _directory = Path.Combine(
        Path.GetTempPath(),
        "nice-aimassist-settings-" + Guid.NewGuid().ToString("N"));

    private readonly List<SettingsStore> _stores = new();

    public SettingsStoreTests()
    {
        Directory.CreateDirectory(_directory);
    }

    public void Dispose()
    {
        foreach (SettingsStore store in _stores)
        {
            store.Dispose();
        }

        Directory.Delete(_directory, recursive: true);
    }

    [Fact]
    public void A_missing_file_is_created_with_the_defaults()
    {
        string path = Path.Combine(_directory, "settings.json");
        SettingsStore store = NewStore(path);

        Settings loaded = store.Current;

        Assert.Equal(new Settings(), loaded);
        Assert.True(File.Exists(path));
        Assert.Equal(new Settings(), Settings.Parse(File.ReadAllText(path)));
    }

    [Fact]
    public void An_unparsable_file_gives_the_defaults_and_is_not_overwritten()
    {
        string path = Path.Combine(_directory, "settings.json");
        const string broken = "{ \"sensitivity_x\": 150";
        File.WriteAllText(path, broken);
        SettingsStore store = NewStore(path);

        Settings loaded = store.Current;

        Assert.Equal(new Settings(), loaded);
        Assert.Equal(broken, File.ReadAllText(path));
    }

    [Fact]
    public void A_valid_edit_is_published_after_the_debounce()
    {
        string path = Path.Combine(_directory, "settings.json");
        SettingsStore store = NewStore(path);

        File.WriteAllText(path, @"{ ""sensitivity_x"": 300 }");

        Assert.True(WaitUntil(() => store.Current.SensitivityX == 300));
    }

    [Fact]
    public void An_invalid_edit_keeps_the_previous_settings()
    {
        string path = Path.Combine(_directory, "settings.json");
        SettingsStore store = NewStore(path);
        File.WriteAllText(path, @"{ ""sensitivity_x"": 300 }");
        Assert.True(WaitUntil(() => store.Current.SensitivityX == 300));

        File.WriteAllText(path, "{ \"sensitivity_x\": 4");
        Thread.Sleep(DebounceMilliseconds * 6);
        Assert.Equal(300, store.Current.SensitivityX);

        File.WriteAllText(path, @"{ ""sensitivity_x"": 400 }");
        Assert.True(WaitUntil(() => store.Current.SensitivityX == 400));
    }

    private SettingsStore NewStore(string path)
    {
        SettingsStore store = new(path, DebounceMilliseconds);
        _stores.Add(store);
        return store;
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
