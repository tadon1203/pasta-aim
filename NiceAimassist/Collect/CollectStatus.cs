namespace NiceAimassist.Collect;

/// <summary>The collection state that the loop shows.</summary>
public readonly record struct CollectStatus(bool Active, long Saved, string? LastError);
