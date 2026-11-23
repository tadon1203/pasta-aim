using Pstm.Detect.Model;

namespace Pstm.Tests.Detect.Model;

public class ModelContractTests
{
    [Theory]
    [InlineData(ModelContract.ElementFloat32, false)]
    [InlineData(ModelContract.ElementFloat16, true)]
    public void StaticNchwInputGivesItsSizeAndPrecision(int elementType, bool fp16)
    {
        ModelInput input = ModelContract.ParseInput([1, 3, 480, 640], elementType);

        Assert.Equal(new ModelInput(640, 480, Precision(fp16)), input);
    }

    [Theory]
    [InlineData(new long[] { 2, 3, 480, 640 })]
    [InlineData(new long[] { 1, 1, 480, 640 })]
    [InlineData(new long[] { 1, 3, -1, 640 })]
    [InlineData(new long[] { 1, 3, 480 })]
    public void DynamicOrNonNchwInputIsRejected(long[] dims)
    {
        Assert.Throws<InvalidOperationException>(() =>
            ModelContract.ParseInput(dims, ModelContract.ElementFloat32));
    }

    [Fact]
    public void UnsupportedInputElementTypeIsRejected()
    {
        Assert.Throws<InvalidOperationException>(() =>
            ModelContract.ParseInput([1, 3, 64, 64], 11));
    }

    [Theory]
    [InlineData(ModelContract.ElementFloat32, false)]
    [InlineData(ModelContract.ElementFloat16, true)]
    public void EndToEndOutputGivesItsRowCountAndPrecision(int elementType, bool fp16)
    {
        ModelOutput output = ModelContract.ParseOutput([1, 300, 6], elementType);

        Assert.Equal(new ModelOutput(OutputLayout.EndToEnd, 300, 6, Precision(fp16)), output);
    }

    [Theory]
    [InlineData(ModelContract.ElementFloat32, false)]
    [InlineData(ModelContract.ElementFloat16, true)]
    public void HeadOutputGivesItsChannelAndAnchorCount(int elementType, bool fp16)
    {
        ModelOutput output = ModelContract.ParseOutput([1, 5, 8400], elementType);

        Assert.Equal(new ModelOutput(OutputLayout.ChannelsFirst, 5, 8400, Precision(fp16)), output);
    }

    [Fact]
    public void HeadOutputAcceptsOneChannelPerClass()
    {
        ModelOutput output = ModelContract.ParseOutput([1, 84, 8400], ModelContract.ElementFloat32);

        Assert.Equal(new ModelOutput(OutputLayout.ChannelsFirst, 84, 8400, TensorPrecision.Fp32), output);
    }

    private static TensorPrecision Precision(bool fp16) => fp16 ? TensorPrecision.Fp16 : TensorPrecision.Fp32;

    [Theory]
    [InlineData(new long[] { 1, 300, 5 })]
    [InlineData(new long[] { 300, 6 })]
    [InlineData(new long[] { 1, 0, 6 })]
    [InlineData(new long[] { 1, -1, 6 })]
    [InlineData(new long[] { 1, 4, 8400 })]
    [InlineData(new long[] { 1, 8400, 5 })]
    [InlineData(new long[] { 1, 5, 5 })]
    public void OutputThatIsNotAStaticEndToEndOrHeadTensorIsRejected(long[] dims)
    {
        Assert.Throws<InvalidOperationException>(() =>
            ModelContract.ParseOutput(dims, ModelContract.ElementFloat32));
    }

    [Fact]
    public void AbsentMetadataIsAccepted()
    {
        ModelContract.ValidateMetadata(null);
    }

    [Fact]
    public void StaticMetadataIsAccepted()
    {
        ModelContract.ValidateMetadata("False");
    }

    [Fact]
    public void DynamicMetadataIsRejected()
    {
        Assert.Throws<InvalidOperationException>(() =>
            ModelContract.ValidateMetadata(" True "));
    }
}
