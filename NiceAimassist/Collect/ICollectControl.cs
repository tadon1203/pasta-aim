namespace NiceAimassist.Collect;

/// <summary>What the engine thread needs from the training-data collector. All members run on the engine thread, except the readers.</summary>
public interface ICollectControl
{
    bool Active { get; }

    long Saved { get; }

    string? LastError { get; }

    /// <summary>Starts or stops collection. Stopping drops a copy that has not been read yet.</summary>
    void SetEnabled(bool enabled);

    void Configure(int size, int perSecond);

    /// <summary>Moves a finished GPU copy to the writer thread. Never waits for the GPU.</summary>
    void CollectFinishedCopy();
}
