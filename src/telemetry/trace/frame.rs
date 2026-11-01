//! The frame line of the frame trace, and the time base of the file.

use serde::Serialize;

use super::header::ClockInfo;
use crate::{pipeline::Detection, telemetry::FrameRecord};

/// The most detections that a frame line holds. `candidate_total` has the full count.
pub(super) const MAX_TRACED_CANDIDATES: usize = 8;

/// `[center_x, center_y, width, height, confidence, class_id]`, in model input pixels.
type DetectionFields = (f32, f32, f32, f32, f32, i32);

/// Makes the times of the file: microseconds from the start of the run.
#[derive(Debug, Clone, Copy)]
pub(super) struct TraceClock {
    start_qpc: i64,
    hz: i64,
}

impl TraceClock {
    pub(super) fn new(info: ClockInfo) -> Self {
        Self {
            start_qpc: info.run_start_qpc,
            hz: info.qpc_hz.max(1),
        }
    }

    /// A time before the start of the run is negative.
    pub(super) fn micros(self, qpc: i64) -> i64 {
        let ticks = i128::from(qpc) - i128::from(self.start_qpc);
        (ticks * 1_000_000 / i128::from(self.hz)) as i64
    }
}

/// One processed frame. Every `*_us` field is in microseconds from the start of the run.
#[derive(Debug, PartialEq, Serialize)]
pub(super) struct FrameLine {
    pub seq: u64,
    pub present_us: i64,
    pub attempt_start_us: i64,
    pub acquired_us: i64,
    pub copy_submitted_us: i64,
    pub ready_us: i64,
    pub inference_start_us: i64,
    pub inference_done_us: i64,
    pub postprocess_done_us: i64,
    pub output_start_us: i64,
    pub output_complete_us: i64,
    /// From the present time to the end of the output.
    pub total_us: i64,
    pub gpu_preprocess_ms: Option<f64>,
    pub gpu_copy_ms: Option<f64>,
    /// The detections with the highest confidence first, at most `MAX_TRACED_CANDIDATES`.
    pub detections: Vec<DetectionFields>,
    pub candidate_total: usize,
    pub selected: Option<DetectionFields>,
    pub target_id: Option<u32>,
    pub target_switched: Option<bool>,
    /// The distance to the aim point, in model input pixels.
    pub aim_error: Option<(f32, f32)>,
    /// The move before rounding.
    pub planned: Option<(f32, f32)>,
    pub sent: Option<(i16, i16)>,
}

impl FrameLine {
    pub(super) fn new(record: &FrameRecord, clock: TraceClock) -> Self {
        let times = record.times;
        let mut detections: Vec<Detection> = record.detections.iter().copied().collect();
        detections.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
        detections.truncate(MAX_TRACED_CANDIDATES);
        Self {
            seq: record.frame_seq,
            present_us: clock.micros(times.present),
            attempt_start_us: clock.micros(times.attempt_start),
            acquired_us: clock.micros(times.acquired),
            copy_submitted_us: clock.micros(times.copy_submitted),
            ready_us: clock.micros(times.ready),
            inference_start_us: clock.micros(times.inference_start),
            inference_done_us: clock.micros(times.inference_done),
            postprocess_done_us: clock.micros(times.postprocess_done),
            output_start_us: clock.micros(record.output_start_qpc),
            output_complete_us: clock.micros(record.output_complete_qpc),
            total_us: clock.micros(record.output_complete_qpc) - clock.micros(times.present),
            gpu_preprocess_ms: record.gpu_preprocess_ms,
            gpu_copy_ms: record.gpu_copy_ms,
            detections: detections.iter().map(detection_fields).collect(),
            candidate_total: record.detections.total(),
            selected: record.selected.as_ref().map(detection_fields),
            target_id: record.track.map(|track| track.id),
            target_switched: record.track.map(|track| track.switched),
            aim_error: record.aim_error,
            planned: record.planned,
            sent: record.sent.map(|sent| (sent.x, sent.y)),
        }
    }
}

fn detection_fields(detection: &Detection) -> DetectionFields {
    (
        detection.center_x,
        detection.center_y,
        detection.box_width,
        detection.box_height,
        detection.confidence,
        detection.class_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::target_track::Track,
        makcu::Movement,
        pipeline::Detections,
        telemetry::{FrameRecord, FrameTimes},
    };

    /// One tick is one millisecond, and the run starts at tick 100.
    fn clock() -> TraceClock {
        TraceClock::new(ClockInfo {
            qpc_hz: 1_000,
            run_start_qpc: 100,
            run_start_unix_ms: 0,
        })
    }

    fn record(present: i64) -> FrameRecord {
        FrameRecord {
            generation: 1,
            frame_seq: 7,
            times: FrameTimes {
                present,
                attempt_start: present + 1,
                acquired: present + 2,
                copy_submitted: present + 3,
                ready: present + 4,
                inference_start: present + 5,
                inference_done: present + 6,
                postprocess_done: present + 7,
            },
            output_start_qpc: present + 8,
            output_complete_qpc: present + 10,
            gpu_preprocess_ms: Some(0.5),
            gpu_copy_ms: None,
            detections: Detections::EMPTY,
            selected: None,
            track: Some(Track {
                id: 3,
                switched: true,
            }),
            aim_error: Some((1.0, -2.0)),
            planned: Some((1.5, -2.0)),
            sent: Some(Movement { x: 2, y: -2 }),
        }
    }

    #[test]
    fn times_are_microseconds_from_the_start_of_the_run() {
        assert_eq!(clock().micros(100), 0);
        assert_eq!(clock().micros(101), 1_000);
        assert_eq!(clock().micros(90), -10_000);
    }

    #[test]
    fn a_frame_line_has_the_stage_times_and_the_total() {
        let line = FrameLine::new(&record(200), clock());
        assert_eq!(line.seq, 7);
        assert_eq!(line.present_us, 100_000);
        assert_eq!(line.ready_us, 104_000);
        assert_eq!(line.output_start_us, 108_000);
        assert_eq!(line.output_complete_us, 110_000);
        assert_eq!(line.total_us, 10_000);
        assert_eq!(line.target_id, Some(3));
        assert_eq!(line.target_switched, Some(true));
        assert_eq!(line.planned, Some((1.5, -2.0)));
        assert_eq!(line.sent, Some((2, -2)));
    }

    #[test]
    fn a_frame_line_keeps_the_most_confident_detections_first() {
        let mut detections = Detections::EMPTY;
        for index in 0..12 {
            let mut detection = Detection::for_test(index as f32, 0.0, 0);
            detection.confidence = index as f32 / 100.0;
            detections.push(detection);
        }
        let mut frame = record(200);
        frame.detections = detections;
        let line = FrameLine::new(&frame, clock());
        assert_eq!(line.candidate_total, 12);
        assert_eq!(line.detections.len(), MAX_TRACED_CANDIDATES);
        assert_eq!(line.detections[0].0, 11.0);
        assert_eq!(line.detections[MAX_TRACED_CANDIDATES - 1].0, 4.0);
    }
}
