using System.Drawing;
using Pstm.Dataset;
using Pstm.Dataset.Storage;

namespace Pstm.Tests.Dataset.Storage;

public sealed class JpegWriterTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), "pstm-dataset-" + Guid.NewGuid().ToString("N"));

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
        JpegWriter writer = new(_root);

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
