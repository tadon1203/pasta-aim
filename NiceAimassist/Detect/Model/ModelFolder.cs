namespace NiceAimassist.Detect.Model;

internal static class ModelFolder
{
    /// <summary>The folder of the model files. The settings window lists the models in that folder.</summary>
    internal static string Location { get; } = Path.Combine(AppContext.BaseDirectory, "models");

    /// <summary>The path of the model file with the given name.</summary>
    internal static string Resolve(string? model)
    {
        if (model is null)
        {
            throw new InvalidOperationException("no model is selected; set \"model\" in settings.json to a .onnx file name in models/");
        }

        string path = Path.Combine(Location, model);
        if (!File.Exists(path))
        {
            throw new InvalidOperationException($"model file not found: {path}");
        }

        return path;
    }
}
