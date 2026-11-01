//! The engine thread: it owns the pipeline, the aim, and the MAKCU link, and runs one frame at a time.

mod capture;
mod frame;
mod makcu;
mod model;
mod recovery;
mod run;

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, Sender, TryRecvError},
    },
    time::{Duration, Instant},
};

use self::{frame::StepOutcome, run::RunControl};
use super::{CapturePhase, Command, EngineEvent, EngineState, status::StatusHub};
use crate::{
    Repaint,
    aim::{Aim, AimSettings},
    clock::Clock,
    clock::Wait,
    output::Output,
    pipeline::{AdapterLuid, DisplayInfo, DxgiCapture, Pipeline, init_thread},
    settings::{self, Confidence, FpsLimit, Interpolation, NewFrameWait, Settings},
    telemetry::{RunCounters, TelemetrySink},
};

/// How often the engine polls MAKCU connection state and sends the counters, when no other event wakes it.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(100);
/// How long the engine spins before a capture deadline. Only the capture deadline spins.
const CAPTURE_SPIN: Duration = Duration::from_millis(1);

/// Settings applied directly to the serial pipeline.
#[derive(Clone, Copy)]
struct Tuning {
    aim: AimSettings,
    /// The Move interpolation. The engine sends it to MAKCU after each connect.
    interpolation: Interpolation,
    confidence: Confidence,
    fps_limit: FpsLimit,
    new_frame_wait: NewFrameWait,
    low_latency: bool,
    frame_trace: bool,
}

/// The channels and handles that the engine thread is started with.
pub(super) struct WorkerLinks {
    pub commands: Receiver<Command>,
    pub events: Sender<EngineEvent>,
    pub state: Arc<Mutex<Arc<EngineState>>>,
    pub repaint: Repaint,
    pub wait: Wait,
    pub qpc: Clock,
    pub telemetry: TelemetrySink,
}

/// What the engine holds for the selected display.
enum CaptureState {
    None,
    /// A capture with no model. No run can start.
    CaptureOnly(Box<DxgiCapture>),
    Ready(Box<Pipeline>),
}

impl CaptureState {
    /// The display that the state captures, if any.
    fn display(&self) -> Option<&DisplayInfo> {
        match self {
            Self::None => None,
            Self::CaptureOnly(capture) => Some(capture.info()),
            Self::Ready(pipeline) => Some(pipeline.display()),
        }
    }

    /// Whether the state holds a working capture.
    fn is_capture_ready(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// Whether a model is loaded, so that a run can start.
    fn has_model(&self) -> bool {
        matches!(self, Self::Ready(_))
    }

    fn pipeline(&self) -> Option<&Pipeline> {
        match self {
            Self::Ready(pipeline) => Some(pipeline),
            _ => None,
        }
    }

    fn pipeline_mut(&mut self) -> Option<&mut Pipeline> {
        match self {
            Self::Ready(pipeline) => Some(pipeline),
            _ => None,
        }
    }
}

struct Worker {
    event_tx: Sender<EngineEvent>,
    repaint: Repaint,
    status: StatusHub,
    qpc: Clock,
    telemetry: TelemetrySink,
    tuning: Tuning,
    /// The display catalog that the engine uses to find the remembered display.
    displays: Vec<DisplayInfo>,

    /// The capture of the selected display, and the pipeline when a model is loaded.
    capture: CaptureState,
    /// The model input size that the output plans against. The capture of a loaded model sets it.
    input_size: (u32, u32),
    /// Wakes the engine for a command, the housekeeping, or the capture deadline.
    wait: Wait,
    capture_deadline: frame::CaptureDeadline,
    /// The counters of the current run, and the last values sent to the Telemetry thread.
    run_counters: RunCounters,
    sent_counters: RunCounters,
    active_model: Option<(String, PathBuf)>,
    next_pipeline_id: u64,
    capture_adapter: u32,
    capture_output: u32,
    selected_adapter_luid: AdapterLuid,

    /// The target and the move that each frame plans.
    aim: Aim,
    /// The MAKCU link, which the planned move is written to.
    output: Output,
    run: RunControl,
    /// The COM apartment of this thread is initialized, so that DXGI can be used.
    com_ready: bool,
}

fn log_pipeline_state(
    state: &'static str,
    pipeline_id: Option<u64>,
    prepare_id: Option<&str>,
    attempt: Option<u8>,
    reason: Option<&str>,
) {
    tracing::info!(
        event = "gpu_pipeline_state",
        state,
        pipeline_id = ?pipeline_id,
        prepare_id = ?prepare_id,
        attempt = ?attempt,
        reason = ?reason,
        "gpu_pipeline_state"
    );
}

pub(super) fn run_worker(links: WorkerLinks, config: Settings) {
    tracing::info!("engine worker started");
    let WorkerLinks {
        commands,
        events,
        state,
        repaint,
        wait,
        qpc,
        telemetry,
    } = links;
    let mut status = StatusHub::new(state, repaint.clone());
    let com_ready = match init_thread() {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(error = %format_args!("{error:#}"), "engine thread COM initialization failed");
            let message = format!("Engine thread COM initialization failed: {error:#}");
            status.edit(|status| {
                status.capture.phase = CapturePhase::Failed;
                status.capture.error = Some(message.clone());
                status.graphics.catalog_error = Some(message);
            });
            status.flush();
            false
        }
    };

    let tuning = Tuning {
        aim: AimSettings {
            sensitivity: config.sensitivity,
            target_y_offset: config.target_y_offset,
            max_delta: config.max_delta,
        },
        interpolation: config.interpolation,
        confidence: config.confidence,
        fps_limit: config.fps_limit,
        new_frame_wait: config.new_frame_wait,
        low_latency: config.low_latency,
        frame_trace: config.frame_trace,
    };
    let mut output = Output::default();
    // A blank port in the file means auto-detect, as `None` does.
    output.set_port(
        config
            .makcu_port
            .clone()
            .filter(|port| !port.trim().is_empty()),
    );
    let _boost = crate::platform::boost_current_thread("engine");
    let mut worker = Worker {
        event_tx: events,
        repaint,
        status,
        qpc,
        telemetry,
        tuning,
        displays: Vec::new(),
        capture: CaptureState::None,
        input_size: (0, 0),
        wait,
        capture_deadline: frame::CaptureDeadline::new(),
        run_counters: RunCounters::default(),
        sent_counters: RunCounters::default(),
        active_model: None,
        next_pipeline_id: 1,
        capture_adapter: 0,
        capture_output: config.display.output,
        selected_adapter_luid: AdapterLuid::from(config.display.adapter),
        aim: Aim::new(tuning.aim),
        output,
        run: RunControl::default(),
        com_ready,
    };

    worker.refresh_capture_catalog();
    worker.initialize_capture();
    worker.connect_makcu();
    if let Some(name) = config.model {
        match settings::model_path(&name) {
            Ok(path) => worker.load_model(false, name, path),
            Err(error) => {
                tracing::error!(model = %name, error = %format_args!("{error:#}"), "configured model could not be resolved");
                worker
                    .status
                    .edit(|status| status.model.error = Some(format!("{error:#}")));
            }
        }
    }
    worker.event_loop(&commands);
    worker.status.flush();
}

impl Worker {
    /// Periodic work that no event triggers: MAKCU connection changes, and the run counters
    /// that the Telemetry thread shows. Each value is sent only when it changed.
    fn housekeeping(&mut self) {
        self.poll_makcu_link();
        if self.run_counters != self.sent_counters {
            self.sent_counters = self.run_counters;
            self.telemetry.counters(self.run_counters);
        }
    }

    fn event_loop(&mut self, rx: &Receiver<Command>) {
        let mut next_housekeeping = Instant::now();
        loop {
            if !self.drain_commands(rx) {
                return;
            }
            let now = Instant::now();
            if now >= next_housekeeping {
                self.housekeeping();
                next_housekeeping = now + HOUSEKEEPING_INTERVAL;
            }
            let mut capture_at = None;
            if let Some(run) = self.run.active {
                if let Some(at) = self.capture_deadline.pending_until(run.mode, now) {
                    capture_at = Some(at);
                } else {
                    let outcome = self.step_frame(rx);
                    // The end of each pass publishes the state, if it changed.
                    self.status.flush();
                    if outcome == StepOutcome::Shutdown {
                        return;
                    }
                    self.capture_deadline
                        .after_step(outcome, self.tuning.fps_limit);
                    continue;
                }
            }
            self.status.flush();
            // The earlier of the housekeeping and the capture deadline wakes the loop.
            let mut deadline = next_housekeeping;
            let mut spin = Duration::ZERO;
            if let Some(capture) = capture_at
                && capture <= deadline
            {
                deadline = capture;
                spin = CAPTURE_SPIN;
            }
            self.wait.until(Some(deadline), spin);
        }
    }

    fn drain_commands(&mut self, rx: &Receiver<Command>) -> bool {
        loop {
            match rx.try_recv() {
                Ok(command) => {
                    if !self.handle_command(command) {
                        return false;
                    }
                }
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => {
                    self.stop_run();
                    return false;
                }
            }
        }
    }

    /// Returns false when the engine should shut down.
    fn handle_command(&mut self, command: Command) -> bool {
        tracing::debug!(?command, "engine command received");
        match command {
            Command::LoadModel { name, path, notify } => self.load_model(notify, name, path),
            Command::SetSensitivity(value) => {
                self.tuning.aim.sensitivity = value;
                self.push_output_settings();
            }
            Command::SetTargetYOffset(value) => {
                self.tuning.aim.target_y_offset = value;
                self.push_output_settings();
            }
            Command::SetMaxDelta(value) => {
                self.tuning.aim.max_delta = value;
                self.push_output_settings();
            }
            Command::SetConfidence(value) => {
                self.tuning.confidence = value;
                if let Some(pipeline) = self.capture.pipeline_mut() {
                    pipeline.set_confidence_threshold(value);
                }
            }
            Command::SetInterpolation(interpolation) => {
                self.tuning.interpolation = interpolation;
                // While disconnected the value only waits: the next connect sends it.
                if self.output.is_connected() {
                    self.send_interpolation();
                }
            }
            Command::SetFpsLimit(value) => {
                self.tuning.fps_limit = value;
                self.capture_deadline.clear();
            }
            Command::SetLowLatency(enabled) => {
                self.tuning.low_latency = enabled;
                if let Some((name, path)) = self.active_model.clone() {
                    self.load_model(false, name, path);
                }
            }
            Command::SetNewFrameWait(value) => {
                self.tuning.new_frame_wait = value;
            }
            Command::SetFrameTrace(enabled) => self.tuning.frame_trace = enabled,
            Command::SetCaptureSelection { adapter, output } => {
                self.set_capture_selection(adapter, output);
            }
            Command::SetContinuous(active) => self.set_continuous(active),
            Command::RunOnce => self.run_once(),
            Command::ReconnectMakcu => self.reconnect_makcu(),
            Command::SetMakcuPort(port) => {
                self.output.set_port(port);
                self.reconnect_makcu();
            }
            Command::RetryCapture => self.retry_suspended_pipeline(),
            Command::RescanDisplays => self.refresh_capture_catalog(),
            Command::Shutdown => {
                tracing::info!("engine worker shutdown requested");
                self.stop_run();
                return false;
            }
        }
        true
    }

    fn push_output_settings(&mut self) {
        self.aim.set_settings(self.tuning.aim);
    }

    fn set_run_error(&mut self, error: String) {
        self.status.edit(|status| status.run.error = Some(error));
    }

    fn emit_event(&self, event: EngineEvent) {
        let _ = self.event_tx.send(event);
        (self.repaint)();
    }

    fn next_pipeline_id(&mut self) -> u64 {
        let id = self.next_pipeline_id;
        self.next_pipeline_id = id.wrapping_add(1).max(1);
        id
    }
}
