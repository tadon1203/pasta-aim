//! The Frame trace of one Run, written as JSON Lines for offline analysis.
//!
//! The Telemetry thread owns the writer. A write that fails stops the trace for the rest of the Run.

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::FrameRecord;
use crate::inference::Detection;

/// Run-level context written once at the top of the file.
pub(crate) struct TraceHeader {
    pub qpc_hz: i64,
    pub input_size: Option<(usize, usize)>,
    pub sensitivity_percent: u16,
    pub target_y_offset_percent: i16,
    pub max_delta: u16,
}

pub(super) struct FrameTraceWriter {
    out: BufWriter<File>,
    path: PathBuf,
}

impl FrameTraceWriter {
    pub(super) fn create(directory: &Path, header: &TraceHeader) -> Result<Self> {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("failed to create {}", directory.display()))?;
        let unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path = directory.join(format!("trace-{unix_ms}.jsonl"));
        let mut out = BufWriter::new(
            File::create(&path).with_context(|| format!("failed to create {}", path.display()))?,
        );
        write_line(&mut out, &header_json(header))?;
        tracing::info!(path = %path.display(), "frame trace started");
        Ok(Self { out, path })
    }

    pub(super) fn record(&mut self, record: &FrameRecord) -> Result<()> {
        write_line(&mut self.out, &record_json(record))
    }

    pub(super) fn flush(&mut self) -> Result<()> {
        self.out.flush().context("failed to flush the frame trace")
    }

    /// Writes the footer with the number of records dropped during the Run, and closes the file.
    pub(super) fn finish(mut self, dropped: u64) -> Result<()> {
        write_line(
            &mut self.out,
            &json!({"type": "footer", "dropped_records": dropped}),
        )?;
        self.flush()?;
        tracing::info!(
            path = %self.path.display(),
            dropped,
            "frame trace finished"
        );
        Ok(())
    }
}

fn write_line(out: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *out, value)?;
    out.write_all(b"\n")?;
    Ok(())
}

fn header_json(header: &TraceHeader) -> Value {
    json!({
        "type": "header",
        "format": 1,
        "qpc_hz": header.qpc_hz,
        "input_size": header.input_size.map(|(w, h)| [w, h]),
        "sensitivity_percent": header.sensitivity_percent,
        "target_y_offset_percent": header.target_y_offset_percent,
        "max_delta": header.max_delta,
    })
}

fn detection_json(detection: &Detection) -> Value {
    json!([
        detection.center_x,
        detection.center_y,
        detection.box_width,
        detection.box_height,
        detection.confidence,
        detection.class_id,
    ])
}

fn record_json(record: &FrameRecord) -> Value {
    json!({
        "type": "frame",
        // The Frame trace has no separate desktop update identity: both keys hold the frame sequence.
        "seq": record.frame_seq,
        "source_seq": record.frame_seq,
        "present_qpc": record.times.present,
        "ready_qpc": record.times.ready,
        "inference_start_qpc": record.times.inference_start,
        "output_start_qpc": record.output_start_qpc,
        "output_complete_qpc": record.output_complete_qpc,
        // [center_x, center_y, width, height, confidence, class_id] in model input pixels.
        "candidates": record.candidates.as_slice().iter().map(detection_json).collect::<Vec<_>>(),
        "candidate_total": record.candidates.total(),
        "selected": record.selected.as_ref().map(detection_json),
        "target_id": record.track.map(|track| track.id),
        "target_switched": record.track.map(|track| track.switched),
        "planned": record.planned.map(|(x, y)| [x, y]),
        "sent": record.sent.map(|sent| [sent.x, sent.y]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::target_track::Track, inference::Candidates, makcu::Movement, telemetry::FrameTimes,
    };

    #[test]
    fn writes_header_frames_and_footer_as_json_lines() {
        let directory =
            std::env::temp_dir().join(format!("nice-aimassist-trace-{}", std::process::id()));
        let header = TraceHeader {
            qpc_hz: 10_000_000,
            input_size: Some((640, 640)),
            sensitivity_percent: 100,
            target_y_offset_percent: 50,
            max_delta: 0,
        };
        let mut writer = FrameTraceWriter::create(&directory, &header).unwrap();
        let path = writer.path.clone();
        writer
            .record(&FrameRecord {
                generation: 1,
                frame_seq: 7,
                times: FrameTimes {
                    present: 1,
                    attempt_start: 0,
                    acquired: 0,
                    copy_submitted: 0,
                    ready: 2,
                    inference_start: 3,
                    inference_done: 3,
                    postprocess_done: 3,
                },
                output_start_qpc: 4,
                output_complete_qpc: 5,
                gpu_preprocess_ms: None,
                gpu_copy_ms: None,
                candidates: Candidates::EMPTY,
                selected: None,
                track: Some(Track {
                    id: 3,
                    switched: true,
                }),
                aim_error: None,
                planned: Some((1.5, -2.0)),
                sent: Some(Movement { x: 2, y: -2 }),
            })
            .unwrap();
        writer.finish(2).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["type"], "header");
        assert_eq!(lines[1]["seq"], 7);
        assert_eq!(lines[1]["source_seq"], 7);
        assert_eq!(lines[1]["present_qpc"], 1);
        assert_eq!(lines[1]["ready_qpc"], 2);
        assert_eq!(lines[1]["inference_start_qpc"], 3);
        assert_eq!(lines[1]["target_id"], 3);
        assert_eq!(lines[1]["sent"], json!([2, -2]));
        assert_eq!(lines[2]["dropped_records"], 2);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
