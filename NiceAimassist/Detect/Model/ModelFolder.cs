namespace NiceAimassist.Detect.Model;

internal static class ModelFolder
{
    /// <summary>The model file and its folder. The settings window lists the models in that folder.</summary>
    internal sealed record ModelLocation(string Directory, string FilePath);

    internal static ModelLocation Resolve(string? model)
    {
        string directory = Path.Combine(AppContext.BaseDirectory, "models");
        System.IO.Directory.CreateDirectory(directory);

        if (model is null)
        {
            throw new InvalidOperationException("no model is selected; set \"model\" in settings.json to a .onnx file name in models/");
        }

        string path = Path.Combine(directory, model);
        if (!File.Exists(path))
        {
            throw new InvalidOperationException($"model file not found: {path}");
        }

        return new ModelLocation(Path.GetDirectoryName(path) ?? directory, path);
    }
}
