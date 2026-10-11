//! The state of the Telemetry thread: it takes the messages, and it has no thread and no clock of its own.
//! The seam for tests: messages in, live values and the Frame trace out.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use super::{
    FrameRecord, LiveTelemetry, RunCounters, RunStart, TelemetryMessage,
    frame_trace::FrameTraceWriter,
    loop_metrics::{LoopFrame, LoopMetrics},
    metrics::{CaptureSourceMetrics, Metrics, PerformanceSnapshot, StageTimings},
};
use crate::{
    engine::{RunGeneration, RunMode},
    graphics::GpuDropReason,
    makcu::{Movement, RawStats},
    timing::QpcClock,
};

/// The capture source rates are made over this window, from the Counters messages.
const RATE_WINDOW: Duration = Duration::from_secs(1);
const CONTINUOUS_LOG_INTERVAL: Duration = Duration::from_secs(1);

pub(super) struct Aggregator {
    qpc: QpcClock,
    logs_dir: PathBuf,
    /// Frame records that the sink could not queue. Shared with the sink.
    dropped: Arc<AtomicU64>,
    metrics: Metrics,
    loop_metrics: LoopMetrics,
    summary: RunSummary,
    generation: Option<RunGeneration>,
    continuous: bool,
    trace: Option<FrameTraceWriter>,
    dropped_at_start: u64,
    window: RateWindow,
    raw: RawStats,
    last_target: Option<(f32, f32, f32, i32)>,
    last_movement: Option<Movement>,
    last_total_ms: Option<f64>,
}

/// The engine's counters when the window started, and the Frame records since then.
struct RateWindow {
    started: Instant,
    base: RunCounters,
    processed: u64,
}

impl RateWindow {
    fn new(started: Instant, base: RunCounters) -> Self {
        Self {
            started,
            base,
            processed: 0,
        }
    }
}

/// Counters for the once-per-second `continuous inference summary` log line.
struct RunSummary {
    started: Instant,
    frames: usize,
    detections: usize,
    last_movement: Option<Movement>,
}

impl RunSummary {
    fn new(started: Instant) -> Self {
        Self {
            started,
            frames: 0,
            detections: 0,
            last_movement: None,
        }
    }

    fn record(&mut self, detected: bool, command: Option<Movement>) {
        self.frames += 1;
        self.detections += usize::from(detected);
        if command.is_some() {
            self.last_movement = command;
        }
    }

    /// Logs and resets once per interval. The snapshot is only built when the line is written.
    fn log_if_due(&mut self, now: Instant, snapshot: impl FnOnce() -> PerformanceSnapshot) {
        let elapsed = now.duration_since(self.started);
        if elapsed < CONTINUOUS_LOG_INTERVAL {
            return;
        }
        if tracing::enabled!(tracing::Level::DEBUG) {
            let performance = snapshot();
            let drops = &performance.gpu_drop_counts;
            tracing::debug!(
                elapsed_ms = elapsed.as_secs_f64() * 1000.0,
                frames = self.frames,
                detections = self.detections,
                last_movement = ?self.last_movement,
                fps = performance.fps,
                capture_p50_ms = performance.capture.p50_ms,
                inference_p50_ms = performance.inference.p50_ms,
                makcu_write_p50_ms = performance.makcu_write.p50_ms,
                pipeline_p50_ms = performance.total.p50_ms,
                stale_generation = drops.get(GpuDropReason::StaleGeneration),
                pipeline_poisoned = drops.get(GpuDropReason::PipelinePoisoned),
                capture_timeouts = performance.capture_source.timeouts,
                "continuous inference summary"
            );
        }
        *self = Self::new(now);
    }
}

impl Aggregator {
    pub(super) fn new(qpc: QpcClock, logs_dir: PathBuf, dropped: Arc<AtomicU64>) -> Self {
        let now = Instant::now();
        Self {
            qpc,
            logs_dir,
            dropped,
            metrics: Metrics::new(),
            loop_metrics: LoopMetrics::new(),
            summary: RunSummary::new(now),
            generation: None,
            continuous: false,
            trace: None,
            dropped_at_start: 0,
            window: RateWindow::new(now, RunCounters::default()),
            raw: RawStats::default(),
            last_target: None,
            last_movement: None,
            last_total_ms: None,
        }
    }

    pub(super) fn handle(&mut self, message: TelemetryMessage, now: Instant) {
        match message {
            TelemetryMessage::RunStarted(start) => self.run_started(start, now),
            TelemetryMessage::Frame(record) => self.frame(record, now),
            TelemetryMessage::Counters(counters) => self.counters(counters, now),
            TelemetryMessage::RawStats(raw) => self.raw = raw,
            TelemetryMessage::FrameDropped(reason) => self.metrics.record_gpu_drop(reason),
            TelemetryMessage::PipelineChanged => self.metrics.mark_cold_next(),
            TelemetryMessage::RunStopped(generation) => self.run_stopped(generation),
            TelemetryMessage::Shutdown => {}
        }
    }

    /// The live values. The percentiles are made here, when they are read.
    pub(super) fn live(&self) -> LiveTelemetry {
        LiveTelemetry {
            last_target: self.last_target,
            last_movement: self.last_movement,
            last_total_ms: self.last_total_ms,
            performance: self.metrics.snapshot(),
            tracking: self.loop_metrics.snapshot(),
            raw: self.raw,
        }
    }

    /// Writes the buffered lines of the Frame trace. A failed write ends the trace for this Run.
    pub(super) fn flush(&mut self) {
        if let Some(trace) = self.trace.as_mut()
            && let Err(error) = trace.flush()
        {
            tracing::warn!(error = %format!("{error:#}"), "frame trace stopped");
            self.trace = None;
        }
    }

    fn run_started(&mut self, start: RunStart, now: Instant) {
        self.close_trace();
        self.generation = Some(start.generation);
        self.continuous = start.mode == RunMode::Continuous;
        self.window = RateWindow::new(now, RunCounters::default());
        self.loop_metrics.reset();
        self.summary = RunSummary::new(now);
        self.dropped_at_start = self.dropped.load(Ordering::Relaxed);
        self.trace = start
            .trace
            .and_then(|header| match FrameTraceWriter::create(&self.logs_dir, &header) {
                Ok(writer) => Some(writer),
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "frame trace could not be started");
                    None
                }
            });
    }

    fn run_stopped(&mut self, generation: RunGeneration) {
        if self.generation != Some(generation) {
            return;
        }
        self.generation = None;
        self.close_trace();
    }

    /// Writes the footer with the records dropped since this Run started, and closes the file.
    fn close_trace(&mut self) {
        let Some(trace) = self.trace.take() else {
            return;
        };
        let dropped = self
            .dropped
            .load(Ordering::Relaxed)
            .saturating_sub(self.dropped_at_start);
        if let Err(error) = trace.finish(dropped) {
            tracing::warn!(error = %format!("{error:#}"), "frame trace footer could not be written");
        }
    }

    fn frame(&mut self, record: FrameRecord, now: Instant) {
        if self.generation != Some(record.generation) {
            return;
        }
        let qpc = self.qpc;
        let times = record.times;
        let timings = StageTimings {
            present_to_gpu_submit_ms: qpc.elapsed_ms(times.present, times.ready),
            present_to_inference_start_ms: qpc.elapsed_ms(times.present, times.inference_start),
            acquire_wait_ms: qpc.elapsed_ms(times.attempt_start, times.acquired),
            capture_ms: qpc.elapsed_ms(times.acquired, times.ready),
            copy_submit_ms: qpc.elapsed_ms(times.acquired, times.copy_submitted),
            gpu_preprocess_ms: record.gpu_preprocess_ms,
            inference_ms: qpc.elapsed_ms(times.inference_start, times.inference_done),
            postprocess_ms: qpc.elapsed_ms(times.inference_done, times.postprocess_done),
            makcu_write_ms: qpc.elapsed_ms(record.output_start_qpc, record.output_complete_qpc),
            total_ms: qpc.elapsed_ms(times.present, record.output_complete_qpc),
        };
        self.metrics.record_qpc(qpc, times.inference_start, timings);
        if let Some(gpu_copy_ms) = record.gpu_copy_ms {
            self.metrics.record_gpu_copy(gpu_copy_ms);
        }
        self.window.processed += 1;
        self.loop_metrics.record(
            qpc,
            LoopFrame {
                present_qpc: times.present,
                output_complete_qpc: record.output_complete_qpc,
                track: record.track,
                error: record.aim_error,
                sent: record.sent.is_some(),
            },
        );
        self.last_target = record
            .aim_error
            .zip(record.selected)
            .map(|((dx, dy), detection)| (dx, dy, detection.confidence, detection.class_id));
        self.last_movement = record.sent;
        self.last_total_ms = Some(timings.total_ms);
        if let Some(trace) = self.trace.as_mut()
            && let Err(error) = trace.record(&record)
        {
            tracing::warn!(error = %format!("{error:#}"), "frame trace stopped");
            self.trace = None;
        }
        if self.continuous {
            self.summary.record(record.selected.is_some(), record.sent);
            self.summary.log_if_due(now, || self.metrics.snapshot());
        }
    }

    /// The capture source rates come from two Counters messages that are at least one window apart.
    fn counters(&mut self, counters: RunCounters, now: Instant) {
        let elapsed = now.duration_since(self.window.started);
        if elapsed < RATE_WINDOW {
            return;
        }
        let seconds = elapsed.as_secs_f64();
        let base = self.window.base;
        let fresh = counters.new_frames.saturating_sub(base.new_frames);
        let accumulated = counters
            .accumulated_frames
            .saturating_sub(base.accumulated_frames);
        self.metrics.update_capture_source(CaptureSourceMetrics {
            fresh_fps: fresh as f64 / seconds,
            processed_fps: self.window.processed as f64 / seconds,
            timeouts: counters.timeouts.saturating_sub(base.timeouts),
            mean_accumulated_frames: if fresh == 0 {
                0.0
            } else {
                accumulated as f64 / fresh as f64
            },
        });
        self.window = RateWindow::new(now, counters);
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::atomic::AtomicU64};

    use serde_json::Value;

    use super::*;
    use crate::{
        engine::target_track::Track,
        inference::Candidates,
        telemetry::{FrameTimes, TraceHeader},
    };

    /// One tick is one millisecond.
    const QPC: QpcClock = QpcClock::test(1_000.0);

    fn aggregator(logs_dir: PathBuf) -> (Aggregator, Arc<AtomicU64>) {
        let dropped = Arc::new(AtomicU64::new(0));
        (
            Aggregator::new(QPC, logs_dir, Arc::clone(&dropped)),
            dropped,
        )
    }

    fn run_start(generation: RunGeneration, trace: Option<TraceHeader>) -> RunStart {
        RunStart {
            generation,
            mode: RunMode::Continuous,
            trace,
        }
    }

    /// A processed frame. The stage boundaries follow `present`, and the output completes `total` ticks after it.
    fn frame(generation: RunGeneration, frame_seq: u64, present: i64, total: i64) -> FrameRecord {
        FrameRecord {
            generation,
            frame_seq,
            times: FrameTimes {
                present,
                attempt_start: present + 10,
                acquired: present + 12,
                copy_submitted: present + 13,
                ready: present + 16,
                inference_start: present + 17,
                inference_done: present + 22,
                postprocess_done: present + 24,
            },
            output_start_qpc: present + 25,
            output_complete_qpc: present + total,
            gpu_preprocess_ms: None,
            gpu_copy_ms: None,
            candidates: Candidates::EMPTY,
            selected: None,
            track: Some(Track {
                id: 1,
                switched: true,
            }),
            aim_error: None,
            planned: None,
            sent: None,
        }
    }

    fn scratch_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "nice-aimassist-telemetry-{name}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn cold_run_stays_apart_after_pipeline_changed() {
        let (mut aggregator, _) = aggregator(scratch_dir("cold"));
        let now = Instant::now();
        aggregator.handle(TelemetryMessage::RunStarted(run_start(1, None)), now);
        aggregator.handle(TelemetryMessage::Frame(frame(1, 1, 0, 30)), now);
        aggregator.handle(TelemetryMessage::Frame(frame(1, 2, 100, 40)), now);
        let performance = aggregator.live().performance;
        assert_eq!(performance.sample_count, 1);
        assert_eq!(performance.cold_run.expect("cold run").total_ms, 30.0);

        aggregator.handle(TelemetryMessage::PipelineChanged, now);
        assert_eq!(aggregator.live().performance.sample_count, 0);
        assert!(aggregator.live().performance.cold_run.is_none());

        aggregator.handle(TelemetryMessage::Frame(frame(1, 3, 200, 50)), now);
        let performance = aggregator.live().performance;
        assert_eq!(performance.sample_count, 0);
        assert_eq!(performance.cold_run.expect("cold run").total_ms, 50.0);
    }

    #[test]
    fn capture_source_rates_come_from_two_counters_one_second_apart() {
        let (mut aggregator, _) = aggregator(scratch_dir("rates"));
        let start = Instant::now();
        aggregator.handle(TelemetryMessage::RunStarted(run_start(1, None)), start);
        aggregator.handle(TelemetryMessage::Frame(frame(1, 1, 0, 30)), start);
        aggregator.handle(TelemetryMessage::Frame(frame(1, 2, 100, 30)), start);
        // Less than one window after the start: no rates yet.
        aggregator.handle(
            TelemetryMessage::Counters(RunCounters {
                new_frames: 5,
                accumulated_frames: 5,
                timeouts: 0,
            }),
            start + Duration::from_millis(500),
        );
        assert_eq!(aggregator.live().performance.capture_source.fresh_fps, 0.0);

        aggregator.handle(
            TelemetryMessage::Counters(RunCounters {
                new_frames: 30,
                accumulated_frames: 60,
                timeouts: 4,
            }),
            start + Duration::from_secs(1),
        );
        let source = aggregator.live().performance.capture_source;
        assert_eq!(source.fresh_fps, 30.0);
        assert_eq!(source.processed_fps, 2.0);
        assert_eq!(source.timeouts, 4);
        assert_eq!(source.mean_accumulated_frames, 2.0);
    }

    #[test]
    fn run_stopped_writes_the_footer_with_the_dropped_count() {
        let directory = scratch_dir("footer");
        let (mut aggregator, dropped) = aggregator(directory.clone());
        let now = Instant::now();
        let header = TraceHeader {
            qpc_hz: 1_000,
            input_size: Some((640, 640)),
            sensitivity_percent: 100,
            target_y_offset_percent: 50,
            max_delta: 0,
        };
        aggregator.handle(
            TelemetryMessage::RunStarted(run_start(1, Some(header))),
            now,
        );
        aggregator.handle(TelemetryMessage::Frame(frame(1, 1, 0, 30)), now);
        dropped.fetch_add(3, Ordering::Relaxed);
        aggregator.handle(TelemetryMessage::RunStopped(1), now);

        let path = fs::read_dir(&directory)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let lines: Vec<Value> = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["type"], "header");
        assert_eq!(lines[1]["seq"], 1);
        assert_eq!(lines[2]["type"], "footer");
        assert_eq!(lines[2]["dropped_records"], 3);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn statistics_come_from_the_ticks_of_a_frame_record() {
        let (mut aggregator, _) = aggregator(scratch_dir("ticks"));
        let now = Instant::now();
        aggregator.handle(TelemetryMessage::RunStarted(run_start(1, None)), now);
        aggregator.handle(TelemetryMessage::Frame(frame(1, 1, 80, 27)), now);
        aggregator.handle(TelemetryMessage::Frame(frame(1, 2, 80, 27)), now);

        let performance = aggregator.live().performance;
        assert_eq!(performance.sample_count, 1);
        assert_eq!(performance.acquire_wait.p50_ms, 2.0);
        assert_eq!(performance.copy_submit.p50_ms, 1.0);
        assert_eq!(performance.capture.p50_ms, 4.0);
        assert_eq!(performance.present_to_gpu_submit.p50_ms, 16.0);
        assert_eq!(performance.present_to_inference_start.p50_ms, 17.0);
        assert_eq!(performance.inference.p50_ms, 5.0);
        assert_eq!(performance.postprocess.p50_ms, 2.0);
        assert_eq!(performance.makcu_write.p50_ms, 2.0);
        assert_eq!(performance.total.p50_ms, 27.0);
    }

    #[test]
    fn frame_records_of_another_run_are_ignored() {
        let (mut aggregator, _) = aggregator(scratch_dir("generation"));
        let now = Instant::now();
        aggregator.handle(TelemetryMessage::RunStarted(run_start(2, None)), now);
        aggregator.handle(TelemetryMessage::Frame(frame(1, 1, 0, 30)), now);
        let live = aggregator.live();
        assert!(live.last_total_ms.is_none());
        assert_eq!(live.performance.cold_run.map(|cold| cold.total_ms), None);
    }
}
