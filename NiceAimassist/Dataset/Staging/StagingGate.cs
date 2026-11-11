namespace NiceAimassist.Dataset.Staging;

// One sample goes Free, Pending, Mapped, Copied, Free. The GPU writes the staging textures only while the state is
// Free or Pending. The writer reads mapped memory until it calls MarkCopied. A GPU copy, a resize, or an unmap
// before MarkCopied corrupts or frees that memory. Only MarkCopied runs on the writer thread.
internal sealed class StagingGate
{
    private const int Free = 0;
    private const int Pending = 1;
    private const int Mapped = 2;
    private const int Copied = 3;

    private int _state;

    public bool CanCopy => Volatile.Read(ref _state) == Free;

    /// <summary>True from the GPU copy until the textures are mapped.</summary>
    public bool IsPending => Volatile.Read(ref _state) == Pending;

    /// <summary>True while the textures are mapped, also after the writer copied them.</summary>
    public bool IsMapped => Volatile.Read(ref _state) is Mapped or Copied;

    public bool IsCopied => Volatile.Read(ref _state) == Copied;

    public void BeginCopy() => Move(Free, Pending, "Copy the desktop: the staging textures are in use");

    /// <summary>Drops a GPU copy that nobody maps.</summary>
    public void CancelCopy() => Move(Pending, Free, "Cancel the copy of the staging textures: no copy is pending");

    public void Map() => Move(Pending, Mapped, "Map the staging textures: no copy is pending");

    /// <summary>Called on the writer thread after the mapped memory is no longer read.</summary>
    public void MarkCopied() => Volatile.Write(ref _state, Copied);

    /// <summary>Called on the loop thread. Frees the textures after the writer copied them.</summary>
    public void Release() => Move(Copied, Free, "Unmap the staging textures: the writer has not copied them");

    /// <summary>Called on the loop thread when the writer never receives the sample.</summary>
    public void CancelMap() => Move(Mapped, Free, "Cancel the map of the staging textures: they are not mapped");

    private void Move(int from, int to, string failure)
    {
        if (Volatile.Read(ref _state) != from)
        {
            throw new InvalidOperationException(failure);
        }

        Volatile.Write(ref _state, to);
    }
}
