use crate::telemetry::FrameTimes;

#[derive(Debug, Clone, Copy)]
pub struct GpuFrameMetadata {
    pub pipeline_id: u64,
    pub generation: u64,
    pub frame_seq: u64,
    /// The stage boundaries of the frame. The capture fills the first five; the inference fills the rest.
    pub times: FrameTimes,
    pub capture_ready_value: u64,
    pub desktop_size: (usize, usize),
    /// A GPU copy time that became ready during this frame. It belongs to an earlier frame.
    pub gpu_copy_ms: Option<f64>,
}
