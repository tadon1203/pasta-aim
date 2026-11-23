namespace Pstm.Detect.Model;

internal enum TensorPrecision
{
    Fp16,
    Fp32,
}

internal readonly record struct ModelInput(int Width, int Height, TensorPrecision Precision)
{
    /// <summary>The size of the NCHW input tensor, with three channels.</summary>
    internal long ByteLength => 3L * Width * Height * ModelContract.ElementBytes(Precision);
}

internal readonly record struct ModelOutput(int Rows, TensorPrecision Precision)
{
    /// <summary>The size of the output tensor, [1, Rows, OutputColumns].</summary>
    internal long ByteLength => (long)Rows * ModelContract.OutputColumns * ModelContract.ElementBytes(Precision);
}

internal static class ModelContract
{
    internal const int ElementFloat32 = 1;
    internal const int ElementFloat16 = 10;

    internal const int OutputColumns = 6;

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
            || dims[2] != OutputColumns
            || dims[1] <= 0
            || dims[1] > int.MaxValue)
        {
            throw new InvalidOperationException(
                $"Expected a static end-to-end output [1, N, 6] (x1, y1, x2, y2, score, class), got [{Format(dims)}]. Export the model with NMS embedded and a fixed input size.");
        }

        return new ModelOutput((int)dims[1], PrecisionOf(elementType, "output"));
    }

    internal static void ValidateMetadata(ModelMetadata metadata)
    {
        if (!metadata.IsStatic)
        {
            throw new InvalidOperationException(
                "The model was exported with dynamic shapes (metadata dynamic=True). Export it static.");
        }

        if (!metadata.HasEmbeddedNms)
        {
            throw new InvalidOperationException(
                "The model is not end-to-end (metadata end2end=False). Export it with NMS embedded.");
        }
    }

    private static TensorPrecision PrecisionOf(int elementType, string role) => elementType switch
    {
        ElementFloat16 => TensorPrecision.Fp16,
        ElementFloat32 => TensorPrecision.Fp32,
        _ => throw new InvalidOperationException(
            $"Unsupported model {role} element type {elementType}; expected FP16 or FP32."),
    };

    private static string Format(ReadOnlySpan<long> dims) => string.Join(", ", dims.ToArray());
}

/// <summary>The optional model metadata. A missing value means that the export did not set it.</summary>
internal readonly record struct ModelMetadata(string? Dynamic, string? EndToEnd, string? Nms)
{
    public bool IsStatic => ParseFlag(Dynamic) != true;

    // end2end=False is acceptable only when nms=True.
    public bool HasEmbeddedNms => ParseFlag(EndToEnd) != false || ParseFlag(Nms) == true;

    private static bool? ParseFlag(string? value) =>
        value is null ? null : value.Trim().Equals("true", StringComparison.OrdinalIgnoreCase);
}
