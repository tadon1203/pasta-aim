using System.Drawing;
using System.Numerics;
using System.Runtime.InteropServices;

namespace NiceAimassist.Pipeline;

/// <summary>The output of one decode pass. It holds the nearest accepted detection and the highest finite score of any row.</summary>
internal readonly record struct DecodeResult(Detection? Nearest, float MaxScore);

internal static class DetectionDecoder
{
    /// <summary>
    /// Decodes the model output in one pass. The typed view reads the buffer in native byte order.
    /// On the Windows x64 targets, that order is little-endian.
    /// </summary>
    internal static DecodeResult Decode(ReadOnlySpan<byte> rows, ModelOutput output, Size input, float confidence)
    {
        if (output.Rows < 0 || rows.Length < output.ByteLength)
        {
            throw new ArgumentException("The output holds fewer rows than declared.", nameof(rows));
        }

        return output.Precision == TensorPrecision.Fp16
            ? DecodeCells(MemoryMarshal.Cast<byte, Half>(rows), output.Rows, input, confidence)
            : DecodeCells(MemoryMarshal.Cast<byte, float>(rows), output.Rows, input, confidence);
    }

    // The conversion to float is resolved per element type at JIT time. Therefore, the loop has no precision branch.
    private static DecodeResult DecodeCells<TElement>(
        ReadOnlySpan<TElement> cells,
        int rowCount,
        Size input,
        float confidence)
        where TElement : unmanaged, INumberBase<TElement>
    {
        float centerX = input.Width * 0.5f;
        float centerY = input.Height * 0.5f;
        bool found = false;
        float bestDistance = 0f;
        float maxScore = 0f;
        Detection best = default;

        for (int row = 0; row < rowCount; row++)
        {
            int cell = row * ModelContract.OutputColumns;
            float x1 = float.CreateTruncating(cells[cell]);
            float y1 = float.CreateTruncating(cells[cell + 1]);
            float x2 = float.CreateTruncating(cells[cell + 2]);
            float y2 = float.CreateTruncating(cells[cell + 3]);
            float score = float.CreateTruncating(cells[cell + 4]);
            float classId = float.CreateTruncating(cells[cell + 5]);

            // The max score ignores the box and the confidence filter. It is taken before them.
            if (float.IsFinite(score) && score > maxScore)
            {
                maxScore = score;
            }

            if (!float.IsFinite(x1) || !float.IsFinite(y1) || !float.IsFinite(x2)
                || !float.IsFinite(y2) || !float.IsFinite(score) || !float.IsFinite(classId))
            {
                continue;
            }

            if (!(score >= confidence) || x2 <= x1 || y2 <= y1)
            {
                continue;
            }

            Detection candidate = new(x1, y1, x2, y2, score, (int)MathF.Round(classId));
            float dx = candidate.CenterX - centerX;
            float dy = candidate.CenterY - centerY;
            float distance = dx * dx + dy * dy;
            if (found && distance >= bestDistance)
            {
                continue;
            }

            found = true;
            bestDistance = distance;
            best = candidate;
        }

        return new DecodeResult(found ? best : null, maxScore);
    }
}
