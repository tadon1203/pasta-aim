namespace NiceAimassist.Collect;

/// <summary>What the loop thread needs from the training-data collector. The loop thread calls it on every loop.</summary>
public interface ICollectControl
{
    /// <summary>
    /// Applies the settings and the hold state. Then moves a finished GPU copy to the writer thread.
    /// Never waits for the GPU.
    /// </summary>
    CollectStatus Update(CollectSettings settings, bool held);
}
