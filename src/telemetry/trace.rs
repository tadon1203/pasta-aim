//! The frame trace of one run, written as JSON Lines for offline analysis.
//!
//! The file has a header line, a `frame` line for each frame record, an `event` line for each trace
//! event, and a footer line. Every time in the file is in microseconds from the start of the run.
//! The Telemetry thread owns the `FrameTrace`. A write that fails stops the trace for the rest of the run.

mod event;
mod frame;
mod header;

pub(crate) use event::TraceEvent;
pub(crate) use header::{
    AppInfo, ClockInfo, MakcuInfo, ModelInfo, RunInfo, SettingsInfo, TraceHeader,
};

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::Serialize;

use self::{
    event::EventFields,
    frame::{FrameLine, TraceClock},
};
use super::FrameRecord;

/// One line of the file. `type` names the kind.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Line<'a> {
    Header(&'a TraceHeader),
    Frame(FrameLine),
    Event(EventLine<'a>),
    Footer(Footer),
}

#[derive(Serialize)]
struct EventLine<'a> {
    t_us: i64,
    #[serde(flatten)]
    fields: EventFields<'a>,
}

#[derive(Serialize)]
struct Footer {
    frames: u64,
    dropped_records: u64,
}

/// The trace of the Telemetry thread. It is idle until a run starts with the trace on.
pub(super) struct FrameTrace {
    logs_dir: PathBuf,
    active: Option<Active>,
}

/// The open file of one run.
struct Active {
    out: BufWriter<File>,
    path: PathBuf,
    clock: TraceClock,
    frames: u64,
}

impl Active {
    fn write(&mut self, line: &Line) -> Result<()> {
        serde_json::to_writer(&mut self.out, line)?;
        self.out.write_all(b"\n")?;
        Ok(())
    }
}

impl FrameTrace {
    pub(super) fn new(logs_dir: PathBuf) -> Self {
        Self {
            logs_dir,
            active: None,
        }
    }

    /// Opens the file of a run and writes the header. A failure leaves the trace idle.
    pub(super) fn start(&mut self, header: &TraceHeader) {
        match Active::create(&self.logs_dir, header) {
            Ok(active) => self.active = Some(active),
            Err(error) => {
                tracing::warn!(error = %format_args!("{error:#}"), "frame trace could not be started");
            }
        }
    }

    pub(super) fn frame(&mut self, record: &FrameRecord) {
        self.with_active(|active| {
            let line = FrameLine::new(record, active.clock);
            active.frames += 1;
            active.write(&Line::Frame(line))
        });
    }

    pub(super) fn event(&mut self, qpc: i64, event: &TraceEvent) {
        self.with_active(|active| {
            let line = EventLine {
                t_us: active.clock.micros(qpc),
                fields: event.fields(),
            };
            active.write(&Line::Event(line))
        });
    }

    /// Writes the buffered lines.
    pub(super) fn flush(&mut self) {
        self.with_active(|active| {
            active
                .out
                .flush()
                .context("failed to flush the frame trace")
        });
    }

    /// Writes the footer and closes the file. `dropped` is the count of messages that the queue
    /// did not take during the run. Safe to call when idle.
    pub(super) fn stop(&mut self, dropped: u64) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        let footer = Line::Footer(Footer {
            frames: active.frames,
            dropped_records: dropped,
        });
        let result = active
            .write(&footer)
            .and_then(|()| active.out.flush().map_err(Into::into));
        match result {
            Ok(()) => tracing::info!(
                path = %active.path.display(),
                frames = active.frames,
                dropped,
                "frame trace finished"
            ),
            Err(error) => {
                tracing::warn!(error = %format_args!("{error:#}"), "frame trace footer could not be written");
            }
        }
    }

    /// Runs `write` on the open file. A failure closes the trace for the rest of the run.
    fn with_active(&mut self, write: impl FnOnce(&mut Active) -> Result<()>) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if let Err(error) = write(active) {
            tracing::warn!(error = %format_args!("{error:#}"), "frame trace stopped");
            self.active = None;
        }
    }
}

impl Active {
    fn create(directory: &Path, header: &TraceHeader) -> Result<Self> {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("failed to create {}", directory.display()))?;
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path = directory.join(format!("trace-{unix_ms}.jsonl"));
        let file =
            File::create(&path).with_context(|| format!("failed to create {}", path.display()))?;
        let mut active = Self {
            out: BufWriter::new(file),
            path,
            clock: TraceClock::new(header.clock),
            frames: 0,
        };
        active.write(&Line::Header(header))?;
        tracing::info!(path = %active.path.display(), "frame trace started");
        Ok(active)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        engine::target_track::Track, makcu::Movement, pipeline::Detections,
        pipeline::GpuDropReason, telemetry::FrameTimes,
    };

    fn scratch_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "nice-aimassist-trace-{name}-{}",
            std::process::id()
        ))
    }

    fn record(frame_seq: u64, present: i64) -> FrameRecord {
        FrameRecord {
            generation: 1,
            frame_seq,
            times: FrameTimes {
                present,
                attempt_start: present,
                acquired: present,
                copy_submitted: present,
                ready: present + 2,
                inference_start: present + 3,
                inference_done: present + 3,
                postprocess_done: present + 3,
            },
            output_start_qpc: present + 4,
            output_complete_qpc: present + 5,
            gpu_preprocess_ms: None,
            gpu_copy_ms: None,
            detections: Detections::EMPTY,
            selected: None,
            track: Some(Track {
                id: 3,
                switched: true,
            }),
            aim_error: None,
            planned: Some((1.5, -2.0)),
            sent: Some(Movement { x: 2, y: -2 }),
        }
    }

    fn read_lines(directory: &Path) -> Vec<Value> {
        let path = std::fs::read_dir(directory)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn a_run_writes_the_header_frames_events_and_footer_as_json_lines() {
        let directory = scratch_dir("lines");
        let mut trace = FrameTrace::new(directory.clone());
        trace.start(&TraceHeader::for_test());
        trace.frame(&record(7, 10));
        trace.event(
            20,
            &TraceEvent::FrameDropped(GpuDropReason::StaleGeneration),
        );
        trace.event(
            30,
            &TraceEvent::MakcuInterpolationFailed {
                reason: "no echo".to_owned(),
            },
        );
        trace.stop(2);

        let lines = read_lines(&directory);
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0]["type"], "header");
        assert_eq!(lines[0]["clock"]["qpc_hz"], 1_000);
        assert_eq!(lines[0]["settings"]["sensitivity_percent"], 100);
        assert_eq!(lines[1]["type"], "frame");
        assert_eq!(lines[1]["seq"], 7);
        assert_eq!(lines[1]["present_us"], 10_000);
        assert_eq!(lines[1]["total_us"], 5_000);
        assert_eq!(lines[1]["sent"], json!([2, -2]));
        assert_eq!(lines[2]["type"], "event");
        assert_eq!(lines[2]["event"], "frame_dropped");
        assert_eq!(lines[2]["reason"], "stale_generation");
        assert_eq!(lines[2]["t_us"], 20_000);
        assert_eq!(lines[3]["event"], "makcu_interpolation_failed");
        assert_eq!(lines[3]["reason"], "no echo");
        assert_eq!(lines[4]["type"], "footer");
        assert_eq!(lines[4]["frames"], 1);
        assert_eq!(lines[4]["dropped_records"], 2);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_idle_trace_ignores_everything_and_writes_no_file() {
        let directory = scratch_dir("idle");
        let mut trace = FrameTrace::new(directory.clone());
        trace.frame(&record(1, 0));
        trace.event(0, &TraceEvent::PipelineChanged);
        trace.flush();
        trace.stop(0);
        assert!(!directory.exists());
    }

    #[test]
    fn a_trace_that_cannot_be_opened_stays_idle() {
        // The "directory" is a file, so it cannot be created.
        let file = std::env::temp_dir().join(format!(
            "nice-aimassist-trace-blocked-{}",
            std::process::id()
        ));
        std::fs::write(&file, "x").unwrap();
        let mut trace = FrameTrace::new(file.clone());
        trace.start(&TraceHeader::for_test());
        assert!(trace.active.is_none());
        trace.frame(&record(1, 0));
        let _ = std::fs::remove_file(&file);
    }
}
