using Pstm.Capture;

namespace Pstm.Samples;

/// <summary>What the loop needs from the training-data recorder. The loop thread calls it, except <see cref="Status"/>.</summary>
public interface ISampleRecorder
{
    /// <summary>The recording state. Any thread can read it.</summary>
    SampleStatus Status { get; }

    /// <summary>Starts or stops the recording. The loop calls it on every pass.</summary>
    void ApplyEnabled(bool enabled);

    /// <summary>
    /// Unmaps the textures the writer copied, and maps a finished GPU copy for the writer thread.
    /// Never waits for the GPU. The loop calls it on every pass.
    /// </summary>
    void AdvancePipeline();

    /// <summary>
    /// Queues a copy of the held desktop frame when a sample is due. Returns false when the copy does not start.
    /// The frame must stay held until this method returns.
    /// </summary>
    bool TryRecord(IFrameSource frames);
}
