namespace Pstm.Detect.Model;

internal static class ModelFolder
{
    /// <summary>The folder of the model files. The settings window lists the models in that folder.</summary>
    internal static string Location { get; } = Path.Combine(AppContext.BaseDirectory, "models");

    /// <summary>The name of the first model file in name order.</summary>
    internal static string FirstModelName()
    {
        string[] names = Names();
        if (names.Length == 0)
        {
            throw new InvalidOperationException($"no model is selected and {Location} holds no .onnx file");
        }

        return names[0];
    }

    /// <summary>The path of the model file with the given name. A null name selects the first model.</summary>
    internal static string Resolve(string? model)
    {
        string path = Path.Combine(Location, model ?? FirstModelName());
        if (!File.Exists(path))
        {
            throw new InvalidOperationException($"model file not found: {path}");
        }

        return path;
    }

    private static string[] Names()
    {
        if (!Directory.Exists(Location))
        {
            return [];
        }

        return
        [
            .. Directory.GetFiles(Location, "*.onnx")
                .Select(Path.GetFileName)
                .OfType<string>()
                .Order(StringComparer.OrdinalIgnoreCase),
        ];
    }
}
