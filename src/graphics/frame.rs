#[derive(Debug, Clone, Copy)]
pub struct GpuFrameMetadata {
    pub pipeline_id: u64,
    pub generation: u64,
    pub frame_seq: u64,
    /// Original desktop update identity.
    pub source_seq: u64,
    pub present_qpc: i64,
    pub ready_qpc: i64,
    pub capture_ready_value: u64,
    pub accumulated_frames: u32,
    pub desktop_size: (usize, usize),
    pub acquire_wait_ms: f64,
    pub capture_ms: f64,
    pub copy_submit_ms: f64,
}
