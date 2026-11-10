using System.Buffers.Binary;

namespace NiceAimassist.Pipeline;

internal static class DetectionDecoder
{
    internal static Detection? SelectNearest(
        ReadOnlySpan<byte> rows,
        TensorPrecision precision,
        int rowCount,
        int inputWidth,
        int inputHeight,
        float confidence)
    {
        int rowBytes = ModelContract.OutputColumns * ModelContract.ElementBytes(precision);
        if (rowCount < 0 || rows.Length < rowCount * rowBytes)
        {
            throw new ArgumentException("The output holds fewer rows than declared.", nameof(rows));
        }

        float centerX = inputWidth * 0.5f;
        float centerY = inputHeight * 0.5f;
        bool found = false;
        float bestDistance = 0f;
        Detection best = default;

        for (int row = 0; row < rowCount; row++)
        {
            ReadOnlySpan<byte> cells = rows.Slice(row * rowBytes, rowBytes);
            float x1 = Read(cells, 0, precision);
            float y1 = Read(cells, 1, precision);
            float x2 = Read(cells, 2, precision);
            float y2 = Read(cells, 3, precision);
            float score = Read(cells, 4, precision);
            float classId = Read(cells, 5, precision);

            if (!float.IsFinite(x1) || !float.IsFinite(y1) || !float.IsFinite(x2)
                || !float.IsFinite(y2) || !float.IsFinite(score) || !float.IsFinite(classId))
            {
                continue;
            }

            if (!(score >= confidence) || x2 <= x1 || y2 <= y1)
            {
                continue;
            }

            float dx = (x1 + x2) * 0.5f - centerX;
            float dy = (y1 + y2) * 0.5f - centerY;
            float distance = dx * dx + dy * dy;
            if (found && distance >= bestDistance)
            {
                continue;
            }

            found = true;
            bestDistance = distance;
            best = new Detection(x1, y1, x2, y2, score, (int)MathF.Round(classId));
        }

        return found ? best : null;
    }

    /// <summary>The highest finite score of any row, whatever its box. Zero when there is none.</summary>
    internal static float MaxScore(ReadOnlySpan<byte> rows, TensorPrecision precision, int rowCount)
    {
        int rowBytes = ModelContract.OutputColumns * ModelContract.ElementBytes(precision);
        float best = 0f;
        for (int row = 0; row < rowCount && (row + 1) * rowBytes <= rows.Length; row++)
        {
            float score = Read(rows.Slice(row * rowBytes, rowBytes), 4, precision);
            if (float.IsFinite(score) && score > best)
            {
                best = score;
            }
        }

        return best;
    }

    private static float Read(ReadOnlySpan<byte> cells, int column, TensorPrecision precision) =>
        precision == TensorPrecision.Fp16
            ? (float)BinaryPrimitives.ReadHalfLittleEndian(cells.Slice(column * 2))
            : BinaryPrimitives.ReadSingleLittleEndian(cells.Slice(column * 4));
}
