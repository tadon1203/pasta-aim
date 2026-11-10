using System.Runtime.InteropServices;
using NiceAimassist.Pipeline;

namespace NiceAimassist.Tests.Pipeline;

public class DetectionDecoderTests
{
    // Input is 100x100, so the center is (50, 50).
    private const int Width = 100;
    private const int Height = 100;

    [Fact]
    public void NearestDetectionToTheCenterWins()
    {
        float[] rows =
        [
            0f, 0f, 20f, 20f, 0.95f, 1f,
            45f, 45f, 55f, 55f, 0.80f, 2f,
        ];

        Detection? target = DetectionDecoder.SelectNearest(Bytes(rows), TensorPrecision.Fp32, 2, Width, Height, 0.25f);

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

        Detection? target = DetectionDecoder.SelectNearest(Bytes(rows), TensorPrecision.Fp32, 2, Width, Height, 0f);

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

        Detection? target = DetectionDecoder.SelectNearest(Bytes(rows), TensorPrecision.Fp32, 2, Width, Height, 0.25f);

        Assert.Equal(2, target!.Value.Class);
    }

    [Fact]
    public void NoRowAboveTheThresholdGivesNoDetection()
    {
        float[] rows = [45f, 45f, 55f, 55f, 0.10f, 1f];

        Assert.Null(DetectionDecoder.SelectNearest(Bytes(rows), TensorPrecision.Fp32, 1, Width, Height, 0.25f));
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

        Detection? target = DetectionDecoder.SelectNearest(Bytes(rows), TensorPrecision.Fp32, 4, Width, Height, 0.25f);

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

        Detection? target = DetectionDecoder.SelectNearest(Bytes(rows), TensorPrecision.Fp32, 2, Width, Height, 0.25f);

        Assert.Equal(2, target!.Value.Class);
    }

    [Fact]
    public void Fp16RowsDecodeToTheSameSelection()
    {
        // Every value is exact in fp16.
        Half[] rows = Array.ConvertAll(
            new float[]
            {
                0f, 0f, 20f, 20f, 0.875f, 1f,
                45f, 45f, 55f, 55f, 0.5f, 2f,
            },
            value => (Half)value);

        Detection? target = DetectionDecoder.SelectNearest(Bytes(rows), TensorPrecision.Fp16, 2, Width, Height, 0.25f);

        Assert.Equal(new Detection(45f, 45f, 55f, 55f, 0.5f, 2), target);
    }

    [Fact]
    public void Fp16CellsAreReadLittleEndian()
    {
        // Bit patterns: 0x3C00 is 1.0 and 0x4900 is 10.0 in fp16, stored low byte first.
        byte[] row =
        [
            0x00, 0x3C, // x1 = 1
            0x00, 0x3C, // y1 = 1
            0x00, 0x49, // x2 = 10
            0x00, 0x49, // y2 = 10
            0x00, 0x3C, // score = 1
            0x00, 0x3C, // class = 1
        ];

        Detection? target = DetectionDecoder.SelectNearest(row, TensorPrecision.Fp16, 1, 2, 2, 0f);

        Assert.Equal(new Detection(1f, 1f, 10f, 10f, 1f, 1), target);
    }

    [Fact]
    public void FewerBytesThanTheDeclaredRowsIsRejected()
    {
        byte[] rows = new byte[23];

        Assert.Throws<ArgumentException>(() =>
            DetectionDecoder.SelectNearest(rows, TensorPrecision.Fp32, 1, Width, Height, 0f));
    }

    [Fact]
    public void MaxScoreIgnoresTheBoxAndNonFiniteScores()
    {
        float[] rows =
        [
            30f, 30f, 20f, 20f, 0.4f, 0f,
            0f, 0f, 10f, 10f, float.NaN, 0f,
            0f, 0f, 10f, 10f, 0.1f, 0f,
        ];

        Assert.Equal(0.4f, DetectionDecoder.MaxScore(Bytes(rows), TensorPrecision.Fp32, 3));
        Assert.Equal(0f, DetectionDecoder.MaxScore([], TensorPrecision.Fp32, 3));
    }

    private static byte[] Bytes<T>(T[] values) where T : unmanaged =>
        MemoryMarshal.AsBytes(values.AsSpan()).ToArray();
}
