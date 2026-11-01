use crate::{
    aim::{Move, Track},
    engine::RunGeneration,
    pipeline::{Detection, Detections},
};

pub use crate::pipeline::FrameTimes;

/// One processed frame. The Telemetry thread makes the statistics, the loop metrics, and the frame trace from it.
#[derive(Debug, Clone, Copy)]
pub struct FrameRecord {
    pub generation: RunGeneration,
    pub frame_seq: u64,
    pub times: FrameTimes,
    pub output_start_qpc: i64,
    pub output_complete_qpc: i64,
    /// Only on a measured frame.
    pub gpu_preprocess_ms: Option<f64>,
    /// A GPU copy time that became ready during this frame. It belongs to an earlier frame.
    pub gpu_copy_ms: Option<f64>,
    pub detections: Detections,
    pub selected: Option<Detection>,
    pub track: Option<Track>,
    /// The distance to the aim point, in model input pixels.
    pub aim_error: Option<(f32, f32)>,
    /// The move before rounding.
    pub planned: Option<(f32, f32)>,
    pub sent: Option<Move>,
}
