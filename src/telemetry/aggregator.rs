//! The state of the Telemetry thread: it takes the messages, and it has no thread and no clock of its own.
//! The seam for tests: messages in, live values and the frame trace out.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use super::{
    FrameRecord, LiveTelemetry, RunCounters, RunStart, TelemetryMessage, TraceEvent,
    loop_metrics::{LoopFrame, LoopMetrics},
    metrics::{CaptureSourceMetrics, Metrics, StageTimings},
    trace::FrameTrace,
};
use crate::{engine::RunGeneration, makcu::Movement, timing::QpcClock};

/// The capture source rates are made over this window, from the Counters messages.
const RATE_WINDOW: Duration = Duration::from_secs(1);

pub(super) struct Aggregator {
    qpc: QpcClock,
    /// Messages that the sink could not queue. Shared with the sink.
    dropped: Arc<AtomicU64>,
    metrics: Metrics,
    loop_metrics: LoopMetrics,
    generation: Option<RunGeneration>,
    trace: FrameTrace,
    /// The value of `dropped` when the run started, and when the last drop event was made.
    dropped_at_start: u64,
    reported_drops: u64,
    window: RateWindow,
    last_target: Option<(f32, f32, f32, i32)>,
    last_movement: Option<Movement>,
    last_total_ms: Option<f64>,
}

/// The engine's counters when the window started, and the frame records since then.
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

impl Aggregator {
    pub(super) fn new(qpc: QpcClock, logs_dir: PathBuf, dropped: Arc<AtomicU64>) -> Self {
        Self {
            qpc,
            dropped,
            metrics: Metrics::new(),
            loop_metrics: LoopMetrics::new(),
            generation: None,
            trace: FrameTrace::new(logs_dir),
            dropped_at_start: 0,
            reported_drops: 0,
            window: RateWindow::new(Instant::now(), RunCounters::default()),
            last_target: None,
            last_movement: None,
            last_total_ms: None,
        }
    }

    pub(super) fn handle(&mut self, message: TelemetryMessage, now: Instant) {
        self.report_dropped_messages();
        match message {
            TelemetryMessage::RunStarted(start) => self.run_started(start, now),
            TelemetryMessage::Frame(record) => self.frame(&record),
            TelemetryMessage::Counters(counters) => self.counters(counters, now),
            TelemetryMessage::FrameDropped { qpc, reason } => {
                self.trace.event(qpc, &TraceEvent::FrameDropped(reason));
            }
            TelemetryMessage::PipelineChanged { qpc } => {
                self.metrics.mark_cold_next();
                self.trace.event(qpc, &TraceEvent::PipelineChanged);
            }
            TelemetryMessage::Event { qpc, event } => self.trace.event(qpc, &event),
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
        }
    }

    /// Writes the buffered lines of the frame trace.
    pub(super) fn flush(&mut self) {
        self.trace.flush();
    }

    /// Makes a trace event for the messages that the queue refused since the last call.
    /// The time of the event is the time of the report, not the time of the drop.
    fn report_dropped_messages(&mut self) {
        let dropped = self.dropped.load(Ordering::Relaxed);
        if dropped > self.reported_drops {
            let count = dropped - self.reported_drops;
            self.reported_drops = dropped;
            self.trace
                .event(QpcClock::now(), &TraceEvent::TelemetryDropped { count });
        }
    }

    fn run_started(&mut self, start: RunStart, now: Instant) {
        self.stop_trace();
        self.generation = Some(start.generation);
        self.window = RateWindow::new(now, RunCounters::default());
        self.loop_metrics.reset();
        self.dropped_at_start = self.dropped.load(Ordering::Relaxed);
        self.reported_drops = self.dropped_at_start;
        if let Some(header) = start.trace {
            self.trace.start(&header);
        }
    }

    fn run_stopped(&mut self, generation: RunGeneration) {
        if self.generation != Some(generation) {
            return;
        }
        self.generation = None;
        self.stop_trace();
    }

    /// Writes the footer with the messages that the queue refused since the run started.
    fn stop_trace(&mut self) {
        let dropped = self
            .dropped
            .load(Ordering::Relaxed)
            .saturating_sub(self.dropped_at_start);
        self.trace.stop(dropped);
    }

    fn frame(&mut self, record: &FrameRecord) {
        if self.generation != Some(record.generation) {
            return;
        }
        let qpc = self.qpc;
        let times = record.times;
        let timings = StageTimings {
            present_to_gpu_submit: qpc.elapsed_ms(times.present, times.ready),
            present_to_inference_start: qpc.elapsed_ms(times.present, times.inference_start),
            acquire_wait: qpc.elapsed_ms(times.attempt_start, times.acquired),
            capture: qpc.elapsed_ms(times.acquired, times.ready),
            copy_submit: qpc.elapsed_ms(times.acquired, times.copy_submitted),
            gpu_preprocess: record.gpu_preprocess_ms,
            inference: qpc.elapsed_ms(times.inference_start, times.inference_done),
            postprocess: qpc.elapsed_ms(times.inference_done, times.postprocess_done),
            makcu_write: qpc.elapsed_ms(record.output_start_qpc, record.output_complete_qpc),
            total: qpc.elapsed_ms(times.present, record.output_complete_qpc),
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
        self.last_total_ms = Some(timings.total);
        self.trace.frame(record);
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
        graphics::GpuDropReason,
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
        RunStart { generation, trace }
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

    fn read_trace(directory: &PathBuf) -> Vec<Value> {
        let path = fs::read_dir(directory)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
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
        assert_eq!(performance.cold_run.expect("cold run").total, 30.0);

        aggregator.handle(TelemetryMessage::PipelineChanged { qpc: 150 }, now);
        assert_eq!(aggregator.live().performance.sample_count, 0);
        assert!(aggregator.live().performance.cold_run.is_none());

        aggregator.handle(TelemetryMessage::Frame(frame(1, 3, 200, 50)), now);
        let performance = aggregator.live().performance;
        assert_eq!(performance.sample_count, 0);
        assert_eq!(performance.cold_run.expect("cold run").total, 50.0);
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
    fn a_run_with_the_trace_on_writes_frames_events_and_the_footer() {
        let directory = scratch_dir("footer");
        let (mut aggregator, dropped) = aggregator(directory.clone());
        let now = Instant::now();
        aggregator.handle(
            TelemetryMessage::RunStarted(run_start(1, Some(TraceHeader::for_test()))),
            now,
        );
        aggregator.handle(TelemetryMessage::Frame(frame(1, 1, 0, 30)), now);
        aggregator.handle(
            TelemetryMessage::FrameDropped {
                qpc: 40,
                reason: GpuDropReason::PipelinePoisoned,
            },
            now,
        );
        aggregator.handle(
            TelemetryMessage::Event {
                qpc: 50,
                event: TraceEvent::MakcuDisconnected,
            },
            now,
        );
        dropped.fetch_add(3, Ordering::Relaxed);
        aggregator.handle(TelemetryMessage::RunStopped(1), now);

        let lines = read_trace(&directory);
        let kinds: Vec<&str> = lines
            .iter()
            .map(|line| line["type"].as_str().unwrap())
            .collect();
        // The three refused messages are reported before the stop is handled.
        assert_eq!(
            kinds,
            ["header", "frame", "event", "event", "event", "footer"]
        );
        assert_eq!(lines[1]["seq"], 1);
        assert_eq!(lines[2]["event"], "frame_dropped");
        assert_eq!(lines[3]["event"], "makcu_disconnected");
        assert_eq!(lines[4]["event"], "telemetry_dropped");
        assert_eq!(lines[4]["count"], 3);
        assert_eq!(lines[5]["dropped_records"], 3);
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
        assert_eq!(performance.acquire_wait.p50, 2.0);
        assert_eq!(performance.copy_submit.p50, 1.0);
        assert_eq!(performance.capture.p50, 4.0);
        assert_eq!(performance.present_to_gpu_submit.p50, 16.0);
        assert_eq!(performance.present_to_inference_start.p50, 17.0);
        assert_eq!(performance.inference.p50, 5.0);
        assert_eq!(performance.postprocess.p50, 2.0);
        assert_eq!(performance.makcu_write.p50, 2.0);
        assert_eq!(performance.total.p50, 27.0);
    }

    #[test]
    fn frame_records_of_another_run_are_ignored() {
        let (mut aggregator, _) = aggregator(scratch_dir("generation"));
        let now = Instant::now();
        aggregator.handle(TelemetryMessage::RunStarted(run_start(2, None)), now);
        aggregator.handle(TelemetryMessage::Frame(frame(1, 1, 0, 30)), now);
        let live = aggregator.live();
        assert!(live.last_total_ms.is_none());
        assert_eq!(live.performance.cold_run.map(|cold| cold.total), None);
    }
}
