using System.Drawing;
using System.Runtime.InteropServices;
using Pstm.Detect;
using Pstm.Detect.Model;
using Pstm.Detect.Pipeline;

namespace Pstm.Tests.Detect.Pipeline;

public class DetectionDecoderTests
{
    // The input is 100x100. The center is (50, 50).
    private const int Width = 100;
    private const int Height = 100;
    private static readonly Size Input = new(Width, Height);

    [Fact]
    public void NearestDetectionToTheCenterWins()
    {
        float[] rows =
        [
            0f, 0f, 20f, 20f, 0.95f, 1f,
            45f, 45f, 55f, 55f, 0.80f, 2f,
        ];

        Detection? target = DetectionDecoder.Decode(Bytes(rows), Rows(2), Input, 0.25f).Nearest;

        Assert.Equal(new Detection(45f, 45f, 55f, 55f, 0.80f, 2), target);
    }

    [Fact]
    public void FirstRowWinsATie()
    {
        float[] rows =
        [
            40f, 45f, 60f, 55f, 0.9f, 1f,
            45f, 40f, 55f, 60f, 0.9f, 2f,
        ];

        Detection? target = DetectionDecoder.Decode(Bytes(rows), Rows(2), Input, 0f).Nearest;

        Assert.Equal(1, target!.Value.Class);
    }

    [Fact]
    public void ScoreBelowTheThresholdIsIgnoredAndTheThresholdItselfIsKept()
    {
        float[] rows =
        [
            45f, 45f, 55f, 55f, 0.24f, 1f,
            0f, 0f, 20f, 20f, 0.25f, 2f,
        ];

        Detection? target = DetectionDecoder.Decode(Bytes(rows), Rows(2), Input, 0.25f).Nearest;

        Assert.Equal(2, target!.Value.Class);
    }

    [Fact]
    public void NoRowAboveTheThresholdGivesNoDetection()
    {
        float[] rows = [45f, 45f, 55f, 55f, 0.10f, 1f];

        Assert.Null(DetectionDecoder.Decode(Bytes(rows), Rows(1), Input, 0.25f).Nearest);
    }

    [Fact]
    public void RowsWithNonFiniteValuesAreSkippedEvenWhenNearest()
    {
        float[] rows =
        [
            50f, 50f, 52f, 52f, float.NaN, 1f,
            50f, 50f, 52f, 52f, 0.9f, float.NaN,
            float.NegativeInfinity, 50f, 52f, 52f, 0.9f, 1f,
            0f, 0f, 20f, 20f, 0.5f, 2f,
        ];

        Detection? target = DetectionDecoder.Decode(Bytes(rows), Rows(4), Input, 0.25f).Nearest;

        Assert.Equal(2, target!.Value.Class);
    }

    [Fact]
    public void EmptyBoxesAreSkipped()
    {
        float[] rows =
        [
            50f, 50f, 50f, 60f, 0.9f, 1f,
            0f, 0f, 20f, 20f, 0.5f, 2f,
        ];

        Detection? target = DetectionDecoder.Decode(Bytes(rows), Rows(2), Input, 0.25f).Nearest;

        Assert.Equal(2, target!.Value.Class);
    }

    [Fact]
    public void Fp16CellsAreReadLittleEndian()
    {
        // In fp16, the bit pattern 0x3C00 is 1.0 and 0x4900 is 10.0. Each value is stored low byte first.
        byte[] row =
        [
            0x00, 0x3C, // x1 = 1
            0x00, 0x3C, // y1 = 1
            0x00, 0x49, // x2 = 10
            0x00, 0x49, // y2 = 10
            0x00, 0x3C, // score = 1
            0x00, 0x3C, // class = 1
        ];

        Detection? target = DetectionDecoder.Decode(row, Rows(1, TensorPrecision.Fp16), new Size(2, 2), 0f).Nearest;

        Assert.Equal(new Detection(1f, 1f, 10f, 10f, 1f, 1), target);
    }

    [Fact]
    public void FewerBytesThanTheDeclaredRowsIsRejected()
    {
        byte[] rows = new byte[23];

        Assert.Throws<ArgumentException>(() =>
            DetectionDecoder.Decode(rows, Rows(1), Input, 0f));
    }

    [Fact]
    public void TopScoreIgnoresTheBoxTheThresholdAndNonFiniteScores()
    {
        float[] rows =
        [
            0f, 0f, 20f, 20f, 0.3f, 1f,
            45f, 45f, 55f, 55f, float.NaN, 2f,
            50f, 50f, 50f, 60f, 0.9f, 3f,
        ];

        DecodeResult result = DetectionDecoder.Decode(Bytes(rows), Rows(3), Input, 0.95f);

        Assert.Null(result.Nearest);
        Assert.Equal(0.9f, result.TopScore);
    }

    [Fact]
    public void HeadBoxCenterAndSizeBecomeCorners()
    {
        float[] cells = HeadCells(classes: 1, cx: [50f], cy: [50f], width: [20f], height: [10f], scores: [[0.9f]]);

        Detection? target = DetectionDecoder.Decode(Bytes(cells), Head(1, 1), Input, 0.25f).Nearest;

        Assert.Equal(new Detection(40f, 45f, 60f, 55f, 0.9f, 0), target);
    }

    [Fact]
    public void HeadClassWithTheHighestScoreWins()
    {
        float[] cells = HeadCells(classes: 2, cx: [50f], cy: [50f], width: [10f], height: [10f], scores: [[0.3f], [0.8f]]);

        Detection? target = DetectionDecoder.Decode(Bytes(cells), Head(2, 1), Input, 0.25f).Nearest;

        Assert.Equal(1, target!.Value.Class);
        Assert.Equal(0.8f, target.Value.Score);
    }

    [Fact]
    public void HeadNearestAnchorToTheCenterWins()
    {
        float[] cells = HeadCells(
            classes: 1,
            cx: [10f, 50f],
            cy: [10f, 50f],
            width: [10f, 10f],
            height: [10f, 10f],
            scores: [[0.9f, 0.5f]]);

        Detection? target = DetectionDecoder.Decode(Bytes(cells), Head(1, 2), Input, 0.25f).Nearest;

        Assert.Equal(new Detection(45f, 45f, 55f, 55f, 0.5f, 0), target);
    }

    [Fact]
    public void HeadAnchorBelowTheThresholdIsIgnored()
    {
        float[] cells = HeadCells(classes: 1, cx: [50f], cy: [50f], width: [10f], height: [10f], scores: [[0.24f]]);

        DecodeResult result = DetectionDecoder.Decode(Bytes(cells), Head(1, 1), Input, 0.25f);

        Assert.Null(result.Nearest);
        Assert.Equal(0.24f, result.TopScore);
    }

    [Fact]
    public void HeadAnchorWithNonFiniteValuesIsSkipped()
    {
        float[] cells = HeadCells(
            classes: 1,
            cx: [float.NaN, 50f],
            cy: [50f, 50f],
            width: [10f, 10f],
            height: [10f, 10f],
            scores: [[0.9f, 0.6f]]);

        Detection? target = DetectionDecoder.Decode(Bytes(cells), Head(1, 2), Input, 0.25f).Nearest;

        Assert.Equal(new Detection(45f, 45f, 55f, 55f, 0.6f, 0), target);
    }

    [Fact]
    public void HeadAnchorWithoutASizeIsSkipped()
    {
        float[] cells = HeadCells(
            classes: 1,
            cx: [50f, 50f],
            cy: [50f, 50f],
            width: [0f, 10f],
            height: [10f, 10f],
            scores: [[0.9f, 0.6f]]);

        Detection? target = DetectionDecoder.Decode(Bytes(cells), Head(1, 2), Input, 0.25f).Nearest;

        Assert.Equal(0.6f, target!.Value.Score);
    }

    private static ModelOutput Rows(int rows, TensorPrecision precision = TensorPrecision.Fp32) =>
        new(OutputLayout.EndToEnd, rows, ModelContract.EndToEndColumns, precision);

    private static ModelOutput Head(int classes, int anchors, TensorPrecision precision = TensorPrecision.Fp32) =>
        new(OutputLayout.ChannelsFirst, ModelContract.BoxChannels + classes, anchors, precision);

    // The values are stored by channel, as a YOLO head outputs them: cx, cy, w, h, then one score channel per class.
    private static float[] HeadCells(
        int classes,
        float[] cx,
        float[] cy,
        float[] width,
        float[] height,
        float[][] scores)
    {
        int anchors = cx.Length;
        float[] cells = new float[(ModelContract.BoxChannels + classes) * anchors];
        cx.CopyTo(cells, 0);
        cy.CopyTo(cells, anchors);
        width.CopyTo(cells, 2 * anchors);
        height.CopyTo(cells, 3 * anchors);
        for (int channel = 0; channel < classes; channel++)
        {
            scores[channel].CopyTo(cells, (ModelContract.BoxChannels + channel) * anchors);
        }

        return cells;
    }

    private static byte[] Bytes<T>(T[] values) where T : unmanaged =>
        MemoryMarshal.AsBytes(values.AsSpan()).ToArray();
}
