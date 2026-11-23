namespace Pstm.Samples;

/// <summary>The recording state that the loop shows.</summary>
public readonly record struct SampleStatus(bool Active, long Saved, string? LastError);
