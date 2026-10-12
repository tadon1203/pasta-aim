//! The Telemetry thread. It takes the Frame records and the Trace events from the engine.
//! It makes the statistics and the Loop metrics, writes the Frame trace, and publishes the live values.
//!
//! The engine never waits for this thread between a capture and its output. The queue is bounded:
//! a Frame record that does not fit is dropped and counted.

mod aggregator;
mod loop_metrics;
mod metrics;
mod record;
mod trace;

pub use loop_metrics::LoopSnapshot;
pub use metrics::{MetricSummary, PerformanceSnapshot};
pub use record::{FrameRecord, FrameTimes};
pub(crate) use trace::{
    AppInfo, ClockInfo, MakcuInfo, ModelInfo, RunInfo, SettingsInfo, TraceEvent, TraceHeader,
};

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use eframe::egui;

use self::aggregator::Aggregator;
use crate::{
    engine::RunGeneration, graphics::GpuDropReason, makcu::Movement, paths, sync::MutexExt,
    timing::QpcClock,
};

const QUEUE_CAPACITY: usize = 8192;
/// The most often the UI sees the live values.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(100);
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// The engine's counters for one Run. They count from the start of the Run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RunCounters {
    pub new_frames: u64,
    pub accumulated_frames: u64,
    pub timeouts: u64,
}

pub(crate) struct RunStart {
    pub generation: RunGeneration,
    /// `Some` when the Frame trace is on for this Run.
    pub trace: Option<TraceHeader>,
}

// The Frame record is sent on every frame. Boxing it would allocate on the frame path, so it is sent by value.
#[allow(clippy::large_enum_variant)]
pub(crate) enum TelemetryMessage {
    RunStarted(RunStart),
    Frame(FrameRecord),
    Counters(RunCounters),
    /// A frame result that was not sent to the output. `qpc` is the time of the drop.
    FrameDropped {
        qpc: i64,
        reason: GpuDropReason,
    },
    /// The next Frame record is the Cold run, and the statistics start again.
    PipelineChanged {
        qpc: i64,
    },
    /// Something that happened in the Run and is not a Frame. `qpc` is the time of it.
    Event {
        qpc: i64,
        event: TraceEvent,
    },
    RunStopped(RunGeneration),
    Shutdown,
}

/// The engine side of the Telemetry thread.
#[derive(Clone)]
pub(crate) struct TelemetrySink {
    tx: Sender<TelemetryMessage>,
    /// Messages that did not fit in the queue. Shared with the Aggregator, which reports the count of each Run in the Frame trace footer.
    dropped: Arc<AtomicU64>,
    /// Gives the time of an event at the moment that the engine reports it.
    qpc: QpcClock,
}

impl TelemetrySink {
    /// Never blocks. A record that does not fit is dropped and counted.
    pub(crate) fn frame(&self, record: FrameRecord) {
        self.try_send(TelemetryMessage::Frame(record));
    }

    /// Never blocks. A message that does not fit is dropped and counted.
    pub(crate) fn counters(&self, counters: RunCounters) {
        self.try_send(TelemetryMessage::Counters(counters));
    }

    /// Never blocks. A message that does not fit is dropped and counted.
    pub(crate) fn frame_dropped(&self, reason: GpuDropReason) {
        self.try_send(TelemetryMessage::FrameDropped {
            qpc: self.qpc.now(),
            reason,
        });
    }

    /// Never blocks. A message that does not fit is dropped and counted.
    pub(crate) fn event(&self, event: TraceEvent) {
        self.try_send(TelemetryMessage::Event {
            qpc: self.qpc.now(),
            event,
        });
    }

    fn try_send(&self, message: TelemetryMessage) {
        if self.tx.try_send(message).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Blocks while the queue is full. See the ticket's Comments: the frame path can reach this through a command.
    pub(crate) fn run_started(&self, start: RunStart) {
        let _ = self.tx.send(TelemetryMessage::RunStarted(start));
    }

    /// Blocks while the queue is full. See the ticket's Comments: the frame path can reach this through a command.
    pub(crate) fn pipeline_changed(&self) {
        let _ = self.tx.send(TelemetryMessage::PipelineChanged {
            qpc: self.qpc.now(),
        });
    }

    /// Blocks while the queue is full. See the ticket's Comments: the frame path can reach this through a command.
    pub(crate) fn run_stopped(&self, generation: RunGeneration) {
        let _ = self.tx.send(TelemetryMessage::RunStopped(generation));
    }

    /// A sink for tests, with the receiving end of its queue.
    #[cfg(test)]
    pub(crate) fn for_test() -> (Self, Receiver<TelemetryMessage>) {
        let (tx, rx) = bounded(QUEUE_CAPACITY);
        (
            Self {
                tx,
                dropped: Arc::new(AtomicU64::new(0)),
                qpc: QpcClock::test(1_000.0),
            },
            rx,
        )
    }
}

/// What the UI shows from the Frame records. All fields are `Copy`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveTelemetry {
    /// `(dx, dy, confidence, class_id)` of the last detection, relative to the model input center.
    pub last_target: Option<(f32, f32, f32, i32)>,
    pub last_movement: Option<Movement>,
    pub last_total_ms: Option<f64>,
    pub performance: PerformanceSnapshot,
    pub tracking: LoopSnapshot,
}

/// The Telemetry thread and the values it publishes. It lives as long as the engine.
pub(crate) struct Telemetry {
    tx: Sender<TelemetryMessage>,
    live: Arc<Mutex<LiveTelemetry>>,
    join: Option<JoinHandle<()>>,
}

impl Telemetry {
    pub(crate) fn spawn(qpc: QpcClock, ui_ctx: egui::Context) -> (Self, TelemetrySink) {
        let (tx, rx) = bounded(QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let live = Arc::new(Mutex::new(LiveTelemetry::default()));
        let aggregator = Aggregator::new(qpc, paths::logs_dir(), Arc::clone(&dropped));
        let thread_live = Arc::clone(&live);
        let join = thread::Builder::new()
            .name("nice-aimassist-telemetry".to_owned())
            .spawn(move || run_telemetry(rx, aggregator, thread_live, ui_ctx))
            .expect("failed to spawn the telemetry thread");
        let sink = TelemetrySink {
            tx: tx.clone(),
            dropped,
            qpc,
        };
        (
            Self {
                tx,
                live,
                join: Some(join),
            },
            sink,
        )
    }

    pub(crate) fn live(&self) -> LiveTelemetry {
        *self.live.lock_recover()
    }

    /// Sends `Shutdown` and joins the thread. The engine calls this after its worker has ended.
    pub(crate) fn stop(&mut self) {
        let _ = self.tx.send(TelemetryMessage::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// The loop of the Telemetry thread. It publishes at most every `PUBLISH_INTERVAL`, and at once when a Run stops.
fn run_telemetry(
    rx: Receiver<TelemetryMessage>,
    mut aggregator: Aggregator,
    live: Arc<Mutex<LiveTelemetry>>,
    ui_ctx: egui::Context,
) {
    let mut last_publish = Instant::now();
    let mut last_flush = Instant::now();
    let mut pending = false;
    loop {
        let urgent = match rx.recv_timeout(PUBLISH_INTERVAL) {
            Ok(TelemetryMessage::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(message) => {
                let urgent = matches!(message, TelemetryMessage::RunStopped(_));
                aggregator.handle(message, Instant::now());
                pending = true;
                urgent
            }
            Err(RecvTimeoutError::Timeout) => false,
        };
        if pending && (urgent || last_publish.elapsed() >= PUBLISH_INTERVAL) {
            *live.lock_recover() = aggregator.live();
            crate::repaint::request_repaint(&ui_ctx);
            last_publish = Instant::now();
            pending = false;
        }
        if last_flush.elapsed() >= FLUSH_INTERVAL {
            aggregator.flush();
            last_flush = Instant::now();
        }
    }
}
