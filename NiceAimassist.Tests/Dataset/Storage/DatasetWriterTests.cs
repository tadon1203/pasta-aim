using System.Drawing;
using NiceAimassist.Dataset;
using NiceAimassist.Dataset.Storage;

namespace NiceAimassist.Tests.Dataset.Storage;

public sealed class DatasetWriterTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), "nice-aimassist-dataset-" + Guid.NewGuid().ToString("N"));

    public void Dispose()
    {
        if (Directory.Exists(_root))
        {
            Directory.Delete(_root, recursive: true);
        }
    }

    [Fact]
    public void WriterSavesTheOriginalAsJpeg()
    {
        DatasetWriter writer = new(_root);

        writer.Write("sample", Solid(160, 90));

        string original = Path.Combine(_root, "original", "sample.jpg");
        Assert.Equal([(byte)0xFF, (byte)0xD8], File.ReadAllBytes(original).Take(2).ToArray());
        using (Image image = Image.FromFile(original))
        {
            Assert.Equal((160, 90), (image.Width, image.Height));
        }

        Assert.Empty(Directory.GetFiles(_root, "*.tmp", SearchOption.AllDirectories));
    }

    private static BgraImage Solid(int width, int height) => new(new byte[width * height * 4], width, height);
}
