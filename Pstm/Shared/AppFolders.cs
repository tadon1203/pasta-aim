namespace Pstm.Shared;

/// <summary>The writable folders of the app. The root is created on first use.</summary>
internal static class AppFolders
{
    internal static string Root { get; } = CreateRoot();

    internal static string Settings => Path.Combine(Root, "settings.json");

    internal static string Logs => Path.Combine(Root, "logs");

    internal static string Samples => Path.Combine(Root, "samples");

    private static string CreateRoot()
    {
        string baseFolder = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        if (string.IsNullOrEmpty(baseFolder))
        {
            baseFolder = AppContext.BaseDirectory;
        }

        string root = Path.Combine(baseFolder, "pstm");
        Directory.CreateDirectory(root);
        return root;
    }
}
