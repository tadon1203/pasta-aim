//! The Telemetry thread. It takes the frame records and the trace events from the engine.
//! It makes the statistics and the loop metrics, writes the frame trace, and publishes the live values.
//!
//! The engine never waits for this thread between a capture and its output. The queue is bounded:
//! a frame record that does not fit is dropped and counted.

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

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};

use self::aggregator::Aggregator;
use crate::{
    Repaint, clock::Clock, engine::RunGeneration, graphics::GpuDropReason, makcu::Movement,
    settings, sync::MutexExt,
};

const QUEUE_CAPACITY: usize = 8192;
/// The most often the UI sees the live values.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(100);
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// The engine's counters for one run. They count from the start of the run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RunCounters {
    pub new_frames: u64,
    pub accumulated_frames: u64,
    pub timeouts: u64,
}

pub(crate) struct RunStart {
    pub generation: RunGeneration,
    /// `Some` when the frame trace is on for this run.
    pub trace: Option<TraceHeader>,
}

// The frame record is sent on every frame. Boxing it would allocate on the frame path, so it is sent by value.
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
    /// The next frame record is the first frame, and the statistics start again.
    PipelineChanged {
        qpc: i64,
    },
    /// Something that happened in the run and is not a frame. `qpc` is the time of it.
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
    tx: SyncSender<TelemetryMessage>,
    /// Messages that did not fit in the queue. Shared with the Aggregator, which reports the count of each run in the frame trace footer.
    dropped: Arc<AtomicU64>,
}

impl TelemetrySink {
    /// Never blocks. A record that does not fit is dropped and counted.
    pub(crate) fn frame(&self, record: &FrameRecord) {
        self.try_send(TelemetryMessage::Frame(*record));
    }

    /// Never blocks. A message that does not fit is dropped and counted.
    pub(crate) fn counters(&self, counters: RunCounters) {
        self.try_send(TelemetryMessage::Counters(counters));
    }

    /// Never blocks. A message that does not fit is dropped and counted.
    pub(crate) fn frame_dropped(&self, reason: GpuDropReason) {
        self.try_send(TelemetryMessage::FrameDropped {
            qpc: Clock::now(),
            reason,
        });
    }

    /// Never blocks. A message that does not fit is dropped and counted.
    pub(crate) fn event(&self, event: TraceEvent) {
        self.try_send(TelemetryMessage::Event {
            qpc: Clock::now(),
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
        let _ = self
            .tx
            .send(TelemetryMessage::PipelineChanged { qpc: Clock::now() });
    }

    /// Blocks while the queue is full. See the ticket's Comments: the frame path can reach this through a command.
    pub(crate) fn run_stopped(&self, generation: RunGeneration) {
        let _ = self.tx.send(TelemetryMessage::RunStopped(generation));
    }

    /// A sink for tests, with the receiving end of its queue.
    #[cfg(test)]
    pub(crate) fn for_test() -> (Self, Receiver<TelemetryMessage>) {
        let (tx, rx) = sync_channel(QUEUE_CAPACITY);
        (
            Self {
                tx,
                dropped: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }
}

/// What the UI shows from the frame records. All fields are `Copy`.
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
    tx: SyncSender<TelemetryMessage>,
    live: Arc<Mutex<LiveTelemetry>>,
    join: Option<JoinHandle<()>>,
}

impl Telemetry {
    pub(crate) fn spawn(qpc: Clock, repaint: Repaint) -> anyhow::Result<(Self, TelemetrySink)> {
        let (tx, rx) = sync_channel(QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let live = Arc::new(Mutex::new(LiveTelemetry::default()));
        let logs_dir = settings::logs_dir()?;
        let mut aggregator = Aggregator::new(qpc, logs_dir, Arc::clone(&dropped));
        let thread_live = Arc::clone(&live);
        let join = thread::Builder::new()
            .name("nice-aimassist-telemetry".to_owned())
            .spawn(move || run_telemetry(&rx, &mut aggregator, &thread_live, &repaint))
            .expect("failed to spawn the telemetry thread");
        let sink = TelemetrySink {
            tx: tx.clone(),
            dropped,
        };
        Ok((
            Self {
                tx,
                live,
                join: Some(join),
            },
            sink,
        ))
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

/// The loop of the Telemetry thread. It publishes at most every `PUBLISH_INTERVAL`, and at once when a run stops.
fn run_telemetry(
    rx: &Receiver<TelemetryMessage>,
    aggregator: &mut Aggregator,
    live: &Mutex<LiveTelemetry>,
    repaint: &Repaint,
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
            repaint();
            last_publish = Instant::now();
            pending = false;
        }
        if last_flush.elapsed() >= FLUSH_INTERVAL {
            aggregator.flush();
            last_flush = Instant::now();
        }
    }
}
