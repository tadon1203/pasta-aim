//! The engine thread: it owns the pipeline, the aim, and the MAKCU link, and runs one frame at a time.

mod capture;
mod frame;
mod model;
mod output;
mod recovery;
mod run;

use std::{
    path::PathBuf,
    sync::{Arc, mpsc::TryRecvError},
    time::{Duration, Instant},
};

use self::{frame::StepOutcome, run::RunControl};
use super::{
    CapturePhase, Command, State,
    stats::{PUBLISH_INTERVAL, Recorder},
};
use crate::{
    aim::{Aim, AimSettings},
    channel::{Message, Receiver, Sender},
    clock::{Clock, Wait},
    output::Output,
    pipeline::{AdapterLuid, DisplayInfo, DxgiCapture, Pipeline, init_thread},
    settings::{self, Confidence, FpsLimit, Interpolation, NewFrameWait, Settings},
};

/// How often the engine polls MAKCU connection state, when no other event wakes it.
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
}

/// The channels and handles that the engine thread is started with.
pub(super) struct WorkerLinks {
    pub commands: Receiver<Command>,
    pub ui: Sender<Message>,
    pub wait: Wait,
    pub clock: Clock,
}

/// What the engine holds for the selected display.
enum Source {
    None,
    /// A capture with no model. No run can start.
    CaptureOnly(Box<DxgiCapture>),
    Ready(Box<Pipeline>),
}

impl Source {
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
    ui: Sender<Message>,
    /// The engine state. `publish` sends it to the UI after an edit.
    state: State,
    state_changed: bool,
    recorder: Recorder,
    /// When the statistics were last sent to the UI.
    stats_sent: Instant,
    clock: Clock,
    tuning: Tuning,
    /// The display catalog that the engine uses to find the remembered display.
    displays: Vec<DisplayInfo>,

    /// The capture of the selected display, and the pipeline when a model is loaded.
    capture: Source,
    /// The model input size that the output plans against. The capture of a loaded model sets it.
    input_size: (u32, u32),
    /// Wakes the engine for a command, the housekeeping, or the capture deadline.
    wait: Wait,
    capture_deadline: frame::CaptureDeadline,
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

pub(super) fn run_worker(links: WorkerLinks, settings: Settings) {
    tracing::info!("engine thread started");
    let WorkerLinks {
        commands,
        ui,
        wait,
        clock,
    } = links;
    let com_error = init_thread().err().map(|error| {
        tracing::error!(error = %format_args!("{error:#}"), "engine thread COM initialization failed");
        format!("Engine thread COM initialization failed: {error:#}")
    });

    let tuning = Tuning {
        aim: AimSettings {
            sensitivity: settings.sensitivity,
            target_y_offset: settings.target_y_offset,
            max_delta: settings.max_delta,
        },
        interpolation: settings.interpolation,
        confidence: settings.confidence,
        fps_limit: settings.fps_limit,
        new_frame_wait: settings.new_frame_wait,
        low_latency: settings.low_latency,
    };
    let mut output = Output::default();
    // A blank port in the file means auto-detect, as `None` does.
    output.set_port(
        settings
            .makcu_port
            .clone()
            .filter(|port| !port.trim().is_empty()),
    );
    let _boost = crate::platform::boost_current_thread("engine");
    let mut worker = Worker {
        ui,
        state: State::default(),
        state_changed: false,
        recorder: Recorder::new(clock),
        stats_sent: Instant::now(),
        clock,
        tuning,
        displays: Vec::new(),
        capture: Source::None,
        input_size: (0, 0),
        wait,
        capture_deadline: frame::CaptureDeadline::new(),
        active_model: None,
        next_pipeline_id: 1,
        capture_adapter: 0,
        capture_output: settings.display.output,
        selected_adapter_luid: AdapterLuid::from(settings.display.adapter),
        aim: Aim::new(tuning.aim),
        output,
        run: RunControl::default(),
        com_ready: com_error.is_none(),
    };
    if let Some(message) = com_error {
        worker.edit(|state| {
            state.capture.phase = CapturePhase::Failed;
            state.capture.error = Some(message.clone());
            state.graphics.catalog_error = Some(message);
        });
        worker.publish();
    }

    worker.refresh_capture_catalog();
    worker.initialize_capture();
    worker.connect_output();
    if let Some(name) = settings.model {
        match settings::model_path(&name) {
            Ok(path) => worker.load_model(false, name, path),
            Err(error) => {
                tracing::error!(model = %name, error = %format_args!("{error:#}"), "configured model could not be resolved");
                worker.edit(|state| state.model.error = Some(format!("{error:#}")));
            }
        }
    }
    worker.event_loop(&commands);
    worker.publish();
}

impl Worker {
    /// Periodic work that no event triggers: MAKCU connection changes.
    fn housekeeping(&mut self) {
        self.poll_output_link();
    }

    /// Sends the state to the UI after an edit. It also sends the statistics when they changed: while
    /// a run is active, not more often than `PUBLISH_INTERVAL`.
    fn publish(&mut self) {
        if std::mem::take(&mut self.state_changed) {
            self.ui.send(Message::State(Arc::new(self.state.clone())));
        }
        let due = self.run.active.is_none() || self.stats_sent.elapsed() >= PUBLISH_INTERVAL;
        if due && self.recorder.take_changed() {
            self.ui.send(Message::Stats(self.recorder.stats()));
            self.stats_sent = Instant::now();
        }
    }

    fn edit(&mut self, edit: impl FnOnce(&mut State)) {
        edit(&mut self.state);
        self.state_changed = true;
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
                    // The end of each pass sends the state, if it changed.
                    self.publish();
                    if outcome == StepOutcome::Shutdown {
                        return;
                    }
                    self.capture_deadline
                        .after_step(outcome, self.tuning.fps_limit);
                    continue;
                }
            }
            self.publish();
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
            Command::SetCaptureSelection { adapter, output } => {
                self.set_capture_selection(adapter, output);
            }
            Command::SetContinuous(active) => self.set_continuous(active),
            Command::RunOnce => self.run_once(),
            Command::ReconnectOutput => self.reconnect_output(),
            Command::SetOutputPort(port) => {
                self.output.set_port(port);
                self.reconnect_output();
            }
            Command::RetryCapture => self.retry_suspended_pipeline(),
            Command::RescanDisplays => self.refresh_capture_catalog(),
            Command::Shutdown => {
                tracing::info!("engine shutdown requested");
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
        self.edit(|state| state.run.error = Some(error));
    }

    fn next_pipeline_id(&mut self) -> u64 {
        let id = self.next_pipeline_id;
        self.next_pipeline_id = id.wrapping_add(1).max(1);
        id
    }
}
