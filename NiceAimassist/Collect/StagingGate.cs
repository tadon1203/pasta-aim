namespace NiceAimassist.Collect;

// The GPU writes the staging textures only while the state is Free. The writer reads mapped memory
// until it calls MarkCopied. A GPU copy, a resize, or an unmap before MarkCopied corrupts or frees that memory.
internal sealed class StagingGate
{
    private const int Free = 0;
    private const int Mapped = 1;
    private const int Copied = 2;

    private int _state;

    public bool CanCopy => Volatile.Read(ref _state) == Free;

    public bool IsCopied => Volatile.Read(ref _state) == Copied;

    public void Map()
    {
        if (Volatile.Read(ref _state) != Free)
        {
            throw new InvalidOperationException("Map the staging textures: they are still mapped");
        }

        Volatile.Write(ref _state, Mapped);
    }

    /// <summary>Called on the writer thread after the mapped memory is no longer read.</summary>
    public void MarkCopied() => Volatile.Write(ref _state, Copied);

    /// <summary>Called on the loop thread. Frees the textures after the writer copied them.</summary>
    public void Release()
    {
        if (!IsCopied)
        {
            throw new InvalidOperationException("Unmap the staging textures: the writer has not copied them");
        }

        Volatile.Write(ref _state, Free);
    }

    /// <summary>Called on the loop thread when the writer never receives the sample.</summary>
    public void CancelMap()
    {
        if (Volatile.Read(ref _state) != Mapped)
        {
            throw new InvalidOperationException("Cancel the map of the staging textures: they are not mapped");
        }

        Volatile.Write(ref _state, Free);
    }
}
