namespace Pstm.Detect.Model;

internal enum TensorPrecision
{
    Fp16,
    Fp32,
}

internal enum OutputLayout
{
    /// <summary>Rows of x1, y1, x2, y2, score, class.</summary>
    EndToEnd,

    /// <summary>Channels of cx, cy, w, h, then one channel per class. Each channel holds one value per anchor.</summary>
    ChannelsFirst,
}

internal readonly record struct ModelInput(int Width, int Height, TensorPrecision Precision)
{
    /// <summary>The size of the NCHW input tensor, with three channels.</summary>
    internal long ByteLength => 3L * Width * Height * ModelContract.ElementBytes(Precision);
}

internal readonly record struct ModelOutput(OutputLayout Layout, int Rows, int Columns, TensorPrecision Precision)
{
    /// <summary>The size of the output tensor, [1, Rows, Columns].</summary>
    internal long ByteLength => (long)Rows * Columns * ModelContract.ElementBytes(Precision);
}

internal static class ModelContract
{
    internal const int ElementFloat32 = 1;
    internal const int ElementFloat16 = 10;

    /// <summary>The number of values in one end-to-end row.</summary>
    internal const int EndToEndColumns = 6;

    /// <summary>The number of box channels of a raw head: cx, cy, w, and h.</summary>
    internal const int BoxChannels = 4;

    internal static int ElementBytes(TensorPrecision precision) =>
        precision == TensorPrecision.Fp16 ? 2 : 4;

    internal static ModelInput ParseInput(ReadOnlySpan<long> dims, int elementType)
    {
        if (dims.Length != 4
            || dims[0] != 1
            || dims[1] != 3
            || dims[2] <= 0
            || dims[3] <= 0
            || dims[2] > int.MaxValue
            || dims[3] > int.MaxValue)
        {
            throw new InvalidOperationException(
                $"Expected a static NCHW input [1, 3, H, W], got [{Format(dims)}].");
        }

        return new ModelInput((int)dims[3], (int)dims[2], PrecisionOf(elementType, "input"));
    }

    internal static ModelOutput ParseOutput(ReadOnlySpan<long> dims, int elementType)
    {
        if (dims.Length != 3
            || dims[0] != 1
            || dims[1] <= 0
            || dims[2] <= 0
            || dims[1] > int.MaxValue
            || dims[2] > int.MaxValue)
        {
            throw new InvalidOperationException(
                $"Expected a static output [1, N, {EndToEndColumns}] (x1, y1, x2, y2, score, class) or "
                + $"[1, {BoxChannels} + classes, N] (cx, cy, w, h, class scores), got [{Format(dims)}].");
        }

        TensorPrecision precision = PrecisionOf(elementType, "output");

        // An end-to-end export puts the six box values on the last axis. A raw YOLO head puts them on the first.
        if (dims[2] == EndToEndColumns)
        {
            return new ModelOutput(OutputLayout.EndToEnd, (int)dims[1], EndToEndColumns, precision);
        }

        // A raw head has one channel per class and many more anchors than channels.
        if (dims[1] <= BoxChannels || dims[1] >= dims[2])
        {
            throw new InvalidOperationException(
                $"Expected a static output [1, N, {EndToEndColumns}] (x1, y1, x2, y2, score, class) or "
                + $"[1, {BoxChannels} + classes, N] (cx, cy, w, h, class scores), got [{Format(dims)}].");
        }

        return new ModelOutput(OutputLayout.ChannelsFirst, (int)dims[1], (int)dims[2], precision);
    }

    /// <summary>Rejects a model whose shapes are not static. The shapes themselves set the output layout.</summary>
    internal static void ValidateMetadata(string? dynamicFlag)
    {
        if (ParseFlag(dynamicFlag) == true)
        {
            throw new InvalidOperationException(
                "The model was exported with dynamic shapes (metadata dynamic=True). Export it static.");
        }
    }

    private static TensorPrecision PrecisionOf(int elementType, string role) => elementType switch
    {
        ElementFloat16 => TensorPrecision.Fp16,
        ElementFloat32 => TensorPrecision.Fp32,
        _ => throw new InvalidOperationException(
            $"Unsupported model {role} element type {elementType}; expected FP16 or FP32."),
    };

    private static bool? ParseFlag(string? value) =>
        value is null ? null : value.Trim().Equals("true", StringComparison.OrdinalIgnoreCase);

    private static string Format(ReadOnlySpan<long> dims) => string.Join(", ", dims.ToArray());
}
