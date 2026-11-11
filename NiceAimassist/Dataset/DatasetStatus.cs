namespace NiceAimassist.Dataset;

/// <summary>The recording state that the loop shows.</summary>
public readonly record struct DatasetStatus(bool Active, long Saved, string? LastError);
