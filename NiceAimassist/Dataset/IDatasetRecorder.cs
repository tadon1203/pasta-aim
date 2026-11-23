using NiceAimassist.Capture;

namespace NiceAimassist.Dataset;

/// <summary>What the loop needs from the training-data recorder. The loop thread calls it, except <see cref="Status"/>.</summary>
public interface IDatasetRecorder
{
    /// <summary>The recording state. Any thread can read it.</summary>
    DatasetStatus Status { get; }

    /// <summary>True when the next frame should be passed to <see cref="Dataset"/>.</summary>
    bool IsDue { get; }

    /// <summary>
    /// Applies the settings and the hotkey state. Then unmaps the textures the writer copied, and maps a finished GPU copy
    /// for the writer thread. Never waits for the GPU. The loop calls it on every pass.
    /// </summary>
    void Update(DatasetSettings settings, bool held);

    /// <summary>Queues the copies of the held desktop frame. The frame must stay held until this method returns.</summary>
    void Record(IFrameSource frames);
}
