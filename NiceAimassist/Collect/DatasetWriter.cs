using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;

namespace NiceAimassist.Collect;

/// <summary>A tightly packed BGRA image. <see cref="Pixels"/> may be longer than the image.</summary>
public readonly record struct BgraImage(byte[] Pixels, int Width, int Height);

/// <summary>Writes one training shot as an Ultralytics-style crop plus a full-display original, both JPEG.</summary>
public sealed class DatasetWriter
{
    public const int JpegQuality = 95;

    private static readonly ImageCodecInfo Jpeg = ImageCodecInfo.GetImageEncoders().First(codec => codec.FormatID == ImageFormat.Jpeg.Guid);

    private readonly string _cropDirectory;
    private readonly string _originalDirectory;

    public DatasetWriter(string root)
    {
        _cropDirectory = Path.Combine(root, "images", "train");
        _originalDirectory = Path.Combine(root, "original");
    }

    public static string FileNameFor(DateTime local) => local.ToString("yyyyMMdd-HHmmss-fff");

    public void Write(string fileNameWithoutExtension, BgraImage crop, BgraImage original)
    {
        Directory.CreateDirectory(_cropDirectory);
        Directory.CreateDirectory(_originalDirectory);
        Save(crop, Path.Combine(_cropDirectory, fileNameWithoutExtension + ".jpg"));
        Save(original, Path.Combine(_originalDirectory, fileNameWithoutExtension + ".jpg"));
    }

    // A half-written file must never appear under its final name.
    private static void Save(BgraImage image, string path)
    {
        string temp = path + ".tmp";
        GCHandle pin = GCHandle.Alloc(image.Pixels, GCHandleType.Pinned);
        try
        {
            // Format32bppRgb ignores the alpha byte, which desktop duplication does not guarantee to be 255.
            using Bitmap bitmap = new(image.Width, image.Height, image.Width * 4, PixelFormat.Format32bppRgb, pin.AddrOfPinnedObject());
            using EncoderParameters parameters = new(1);
            parameters.Param[0] = new EncoderParameter(Encoder.Quality, (long)JpegQuality);
            bitmap.Save(temp, Jpeg, parameters);
        }
        catch
        {
            File.Delete(temp);
            throw;
        }
        finally
        {
            pin.Free();
        }

        File.Move(temp, path, overwrite: true);
    }
}
