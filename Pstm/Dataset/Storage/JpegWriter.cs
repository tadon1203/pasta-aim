using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;

namespace Pstm.Dataset.Storage;

internal sealed class JpegWriter
{
    public const int JpegQuality = 95;

    private static readonly ImageCodecInfo Jpeg = ImageCodecInfo.GetImageEncoders().First(codec => codec.FormatID == ImageFormat.Jpeg.Guid);

    private readonly string _directory;

    public JpegWriter(string root) => _directory = Path.Combine(root, "original");

    public static string FileNameFor(DateTime local) => local.ToString("yyyyMMdd-HHmmss-fff");

    public void Write(string fileNameWithoutExtension, BgraImage image)
    {
        Directory.CreateDirectory(_directory);
        Save(image, Path.Combine(_directory, fileNameWithoutExtension + ".jpg"));
    }

    // A half-written file must never appear under its final name.
    private static void Save(BgraImage image, string path)
    {
        string temp = path + ".tmp";
        GCHandle pin = GCHandle.Alloc(image.Pixels, GCHandleType.Pinned);
        try
        {
            // Format32bppRgb ignores the alpha byte. Desktop duplication does not guarantee that the alpha byte is 255.
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
