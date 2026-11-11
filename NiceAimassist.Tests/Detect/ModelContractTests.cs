using NiceAimassist.Detect;

namespace NiceAimassist.Tests.Detect;

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

        Assert.Equal(new ModelOutput(300, Precision(fp16)), output);
    }

    private static TensorPrecision Precision(bool fp16) => fp16 ? TensorPrecision.Fp16 : TensorPrecision.Fp32;

    [Theory]
    [InlineData(new long[] { 1, 300, 5 })]
    [InlineData(new long[] { 300, 6 })]
    [InlineData(new long[] { 1, 0, 6 })]
    [InlineData(new long[] { 1, -1, 6 })]
    public void OutputThatIsNotAStaticEndToEndTensorIsRejected(long[] dims)
    {
        Assert.Throws<InvalidOperationException>(() =>
            ModelContract.ParseOutput(dims, ModelContract.ElementFloat32));
    }

    [Fact]
    public void AbsentMetadataIsAccepted()
    {
        ModelContract.ValidateMetadata(null, null, null);
    }

    [Fact]
    public void DynamicMetadataIsRejected()
    {
        Assert.Throws<InvalidOperationException>(() =>
            ModelContract.ValidateMetadata(" True ", null, null));
    }

    [Fact]
    public void NonEndToEndMetadataIsRejectedUnlessNmsIsEmbedded()
    {
        Assert.Throws<InvalidOperationException>(() =>
            ModelContract.ValidateMetadata("False", "False", null));
        Assert.Throws<InvalidOperationException>(() =>
            ModelContract.ValidateMetadata("False", "False", "False"));

        ModelContract.ValidateMetadata("False", "False", "true");
    }
}
