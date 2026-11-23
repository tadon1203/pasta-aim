using System.Drawing;
using System.Numerics;
using System.Runtime.InteropServices;
using Pstm.Detect.Model;

namespace Pstm.Detect.Pipeline;

/// <summary>The output of one decode pass. It holds the nearest accepted detection and the highest finite score of any value.</summary>
internal readonly record struct DecodeResult(Detection? Nearest, float TopScore);

internal static class DetectionDecoder
{
    /// <summary>
    /// Decodes the model output in one pass. The typed view reads the buffer in native byte order.
    /// On the Windows x64 targets, that order is little-endian.
    /// </summary>
    internal static DecodeResult Decode(ReadOnlySpan<byte> output, ModelOutput shape, Size input, float threshold)
    {
        if (output.Length < shape.ByteLength)
        {
            throw new ArgumentException("The output holds fewer values than declared.", nameof(output));
        }

        return shape.Precision == TensorPrecision.Fp16
            ? DecodeCells(MemoryMarshal.Cast<byte, Half>(output), shape, input, threshold)
            : DecodeCells(MemoryMarshal.Cast<byte, float>(output), shape, input, threshold);
    }

    // The conversion to float is resolved per element type at JIT time. Therefore, the loop has no precision branch.
    private static DecodeResult DecodeCells<TElement>(
        ReadOnlySpan<TElement> cells,
        ModelOutput shape,
        Size input,
        float threshold)
        where TElement : unmanaged, INumberBase<TElement>
    {
        if (shape.Layout == OutputLayout.EndToEnd)
        {
            return DecodeRows(cells, shape.Rows, shape.Columns, input, threshold);
        }

        return DecodeHead(cells, shape.Rows, shape.Columns, input, threshold);
    }

    // Each row holds x1, y1, x2, y2, score, and class.
    private static DecodeResult DecodeRows<TElement>(
        ReadOnlySpan<TElement> cells,
        int rowCount,
        int columns,
        Size input,
        float threshold)
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
            int cell = row * columns;
            float x1 = float.CreateTruncating(cells[cell]);
            float y1 = float.CreateTruncating(cells[cell + 1]);
            float x2 = float.CreateTruncating(cells[cell + 2]);
            float y2 = float.CreateTruncating(cells[cell + 3]);
            float score = float.CreateTruncating(cells[cell + 4]);
            float classId = float.CreateTruncating(cells[cell + 5]);

            // The max score ignores the box and the threshold filter. It is taken before them.
            if (float.IsFinite(score) && score > maxScore)
            {
                maxScore = score;
            }

            if (!float.IsFinite(x1) || !float.IsFinite(y1) || !float.IsFinite(x2)
                || !float.IsFinite(y2) || !float.IsFinite(score) || !float.IsFinite(classId))
            {
                continue;
            }

            if (!(score >= threshold) || x2 <= x1 || y2 <= y1)
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

    // Each channel holds one value per anchor: cx, cy, w, h, then one class score per class.
    // The channel order is the YOLO head layout that Ultralytics exports without NMS.
    private static DecodeResult DecodeHead<TElement>(
        ReadOnlySpan<TElement> cells,
        int channels,
        int anchors,
        Size input,
        float threshold)
        where TElement : unmanaged, INumberBase<TElement>
    {
        float centerX = input.Width * 0.5f;
        float centerY = input.Height * 0.5f;
        bool found = false;
        float bestDistance = 0f;
        float maxScore = 0f;
        Detection best = default;

        int firstClass = ModelContract.BoxChannels;
        for (int anchor = 0; anchor < anchors; anchor++)
        {
            float score = float.NegativeInfinity;
            int bestClass = 0;
            for (int channel = firstClass; channel < channels; channel++)
            {
                float value = float.CreateTruncating(cells[channel * anchors + anchor]);
                if (!float.IsFinite(value) || value <= score)
                {
                    continue;
                }

                score = value;
                bestClass = channel - firstClass;
            }

            // The max score ignores the box and the threshold filter. It is taken before them.
            if (float.IsFinite(score) && score > maxScore)
            {
                maxScore = score;
            }

            if (!(score >= threshold))
            {
                continue;
            }

            float cx = float.CreateTruncating(cells[anchor]);
            float cy = float.CreateTruncating(cells[anchors + anchor]);
            float width = float.CreateTruncating(cells[2 * anchors + anchor]);
            float height = float.CreateTruncating(cells[3 * anchors + anchor]);
            if (!float.IsFinite(cx) || !float.IsFinite(cy) || !float.IsFinite(width) || !float.IsFinite(height))
            {
                continue;
            }

            float halfWidth = width * 0.5f;
            float halfHeight = height * 0.5f;
            if (halfWidth <= 0f || halfHeight <= 0f)
            {
                continue;
            }

            Detection candidate = new(
                cx - halfWidth,
                cy - halfHeight,
                cx + halfWidth,
                cy + halfHeight,
                score,
                bestClass);
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
