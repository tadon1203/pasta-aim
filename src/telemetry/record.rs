use crate::{
    engine::{RunGeneration, target_track::Track},
    inference::{Candidates, Detection},
    makcu::Movement,
};

/// The stage boundaries of one frame, as QPC ticks.
#[derive(Debug, Clone, Copy)]
pub struct FrameTimes {
    /// The Present time of the desktop update.
    pub present: i64,
    /// The capture attempt started, just before `AcquireNextFrame`.
    pub attempt_start: i64,
    /// The desktop image was acquired.
    pub acquired: i64,
    /// The copy to the shared texture was recorded.
    pub copy_submitted: i64,
    /// The capture-ready fence was signalled and flushed.
    pub ready: i64,
    /// Just before the preprocess is submitted.
    pub inference_start: i64,
    /// After the ONNX Runtime run.
    pub inference_done: i64,
    /// After the postprocess.
    pub postprocess_done: i64,
}

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
    pub candidates: Candidates,
    pub selected: Option<Detection>,
    pub track: Option<Track>,
    /// The distance to the aim point, in model input pixels.
    pub aim_error: Option<(f32, f32)>,
    /// The move before rounding.
    pub planned: Option<(f32, f32)>,
    pub sent: Option<Movement>,
}
