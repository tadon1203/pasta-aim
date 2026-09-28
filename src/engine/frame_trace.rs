//! Per-frame trace of the tracking loop, written as JSON Lines for offline analysis.
//!
//! The inference thread only calls `FrameTraceWriter::record` (a non-blocking
//! `try_send`); a separate thread does all formatting and file I/O. When the
//! queue is full the record is dropped and counted rather than waiting.

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use serde_json::{Value, json};

use super::target_track::Track;
use crate::inference::{Candidates, Detection};

const QUEUE_CAPACITY: usize = 8192;
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// Run-level context written once at the top of the file.
pub(super) struct TraceHeader {
    pub qpc_hz: i64,
    pub input_size: Option<(usize, usize)>,
    pub sensitivity_percent: u16,
    pub target_y_offset_percent: i16,
    pub max_delta: u16,
}

/// One processed frame. `Copy` so recording never allocates.
#[derive(Clone, Copy)]
pub(super) struct FrameTraceRecord {
    pub frame_seq: u64,
    pub present_qpc: i64,
    pub ready_qpc: i64,
    pub inference_start_qpc: i64,
    pub output_start_qpc: i64,
    pub output_complete_qpc: i64,
    pub candidates: Candidates,
    pub selected: Option<Detection>,
    pub track: Option<Track>,
    /// Movement before rounding (after sensitivity and the max delta cap).
    pub planned: Option<(f32, f32)>,
    /// Movement actually submitted.
    pub sent: Option<(i32, i32)>,
    /// Frames dropped by the causal gate since the run started.
    pub gate_discards_total: u64,
}

enum Message {
    Record(Box<FrameTraceRecord>),
    Close,
}

pub(super) struct FrameTraceWriter {
    tx: Sender<Message>,
    dropped: Arc<AtomicU64>,
    join: Option<JoinHandle<()>>,
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

        let (tx, rx) = bounded(QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let writer_dropped = Arc::clone(&dropped);
        let join = thread::Builder::new()
            .name("nice-aimassist-trace".to_owned())
            .spawn(move || write_loop(rx, out, &writer_dropped))?;
        tracing::info!(path = %path.display(), "frame trace started");
        Ok(Self {
            tx,
            dropped,
            join: Some(join),
            path,
        })
    }

    pub(super) fn record(&self, record: FrameTraceRecord) {
        if self.tx.try_send(Message::Record(Box::new(record))).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Drop for FrameTraceWriter {
    fn drop(&mut self) {
        // Blocking send is fine here: this is the stop path, not the frame path.
        let _ = self.tx.send(Message::Close);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        tracing::info!(
            path = %self.path.display(),
            dropped = self.dropped.load(Ordering::Relaxed),
            "frame trace finished"
        );
    }
}

fn write_loop(rx: Receiver<Message>, mut out: BufWriter<File>, dropped: &AtomicU64) {
    loop {
        match rx.recv_timeout(FLUSH_INTERVAL) {
            Ok(Message::Record(record)) => {
                if write_line(&mut out, &record_json(&record)).is_err() {
                    return;
                }
            }
            Ok(Message::Close) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                if out.flush().is_err() {
                    return;
                }
            }
        }
    }
    let _ = write_line(
        &mut out,
        &json!({"type": "footer", "dropped_records": dropped.load(Ordering::Relaxed)}),
    );
    let _ = out.flush();
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

fn record_json(record: &FrameTraceRecord) -> Value {
    json!({
        "type": "frame",
        "seq": record.frame_seq,
        "present_qpc": record.present_qpc,
        "ready_qpc": record.ready_qpc,
        "inference_start_qpc": record.inference_start_qpc,
        "output_start_qpc": record.output_start_qpc,
        "output_complete_qpc": record.output_complete_qpc,
        // [center_x, center_y, width, height, confidence, class_id] in model input pixels.
        "candidates": record.candidates.as_slice().iter().map(detection_json).collect::<Vec<_>>(),
        "candidate_total": record.candidates.total(),
        "selected": record.selected.as_ref().map(detection_json),
        "target_id": record.track.map(|track| track.id),
        "target_switched": record.track.map(|track| track.switched),
        "planned": record.planned.map(|(x, y)| [x, y]),
        "sent": record.sent.map(|(x, y)| [x, y]),
        "gate_discards_total": record.gate_discards_total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let writer = FrameTraceWriter::create(&directory, &header).unwrap();
        let path = writer.path.clone();
        writer.record(FrameTraceRecord {
            frame_seq: 7,
            present_qpc: 1,
            ready_qpc: 2,
            inference_start_qpc: 3,
            output_start_qpc: 4,
            output_complete_qpc: 5,
            candidates: Candidates::EMPTY,
            selected: None,
            track: Some(Track {
                id: 3,
                switched: true,
            }),
            planned: Some((1.5, -2.0)),
            sent: Some((2, -2)),
            gate_discards_total: 4,
        });
        drop(writer);

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["type"], "header");
        assert_eq!(lines[1]["seq"], 7);
        assert_eq!(lines[1]["target_id"], 3);
        assert_eq!(lines[1]["sent"], json!([2, -2]));
        assert_eq!(lines[2]["dropped_records"], 0);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
