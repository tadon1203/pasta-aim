//! The engine thread: it owns the Pipeline and the MAKCU client, and runs one frame at a time.

mod capture;
mod frame;
mod makcu;
mod model;
mod recovery;
mod run;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc::Sender},
    time::{Duration, Instant},
};

use crossbeam_channel::{Receiver, TryRecvError};
use eframe::egui;

use self::{frame::StepOutcome, run::RunControl};
use super::{
    CapturePhase, Command, EngineEvent, EngineState,
    output::{OutputSettings, OutputStage},
    status::StatusHub,
};
use crate::{
    config::Config,
    graphics::{DisplayInfo, DxgiCapture, GraphicsThread},
    inference::AdapterLuid,
    makcu::MakcuClient,
    models,
    pipeline::Pipeline,
    repaint,
    settings::{CaptureSettings, ConfidenceThreshold, FpsLimit},
    telemetry::{RunCounters, TelemetrySink, TraceEvent},
    timing::QpcClock,
    wait::DeadlineWait,
};

/// How often the engine polls MAKCU connection state and sends the counters, when no other event wakes it.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(100);
/// How long the engine spins before a capture deadline. Only the capture deadline spins.
const CAPTURE_SPIN: Duration = Duration::from_millis(1);

/// Settings applied directly to the serial pipeline.
#[derive(Clone, Copy)]
struct Tuning {
    output: OutputSettings,
    confidence_threshold: ConfidenceThreshold,
    fps_limit: FpsLimit,
    capture: CaptureSettings,
    low_latency: bool,
    frame_trace: bool,
}

/// The channels and handles that the engine thread is started with.
pub(super) struct WorkerLinks {
    pub commands: Receiver<Command>,
    pub events: Sender<EngineEvent>,
    pub state: Arc<Mutex<Arc<EngineState>>>,
    pub ui_ctx: egui::Context,
    pub wait: DeadlineWait,
    pub qpc: QpcClock,
    pub telemetry: TelemetrySink,
}

/// What the engine holds for the selected display.
enum CaptureState {
    None,
    /// A capture with no model. No Run can start.
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

    /// Whether a model is loaded, so that a Run can start.
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

    fn pipeline_id(&self) -> Option<u64> {
        self.pipeline().map(Pipeline::id)
    }
}

struct Worker {
    event_tx: Sender<EngineEvent>,
    ui_ctx: egui::Context,
    status: StatusHub,
    qpc: QpcClock,
    telemetry: TelemetrySink,
    tuning: Tuning,
    /// The display catalog that the engine uses to find the remembered display.
    displays: Vec<DisplayInfo>,

    /// The capture of the selected display, and the Pipeline when a model is loaded.
    capture: CaptureState,
    output_stage: OutputStage,
    /// Wakes the engine for a command, the housekeeping, or the capture deadline.
    wait: DeadlineWait,
    capture_deadline: frame::CaptureDeadline,
    /// The counters of the current Run, and the last values sent to the Telemetry thread.
    run_counters: RunCounters,
    sent_counters: RunCounters,
    active_model: Option<(String, PathBuf)>,
    auto_reprepare_attempted: bool,
    next_pipeline_id: u64,
    capture_adapter: u32,
    capture_output: u32,
    selected_adapter_luid: AdapterLuid,

    makcu: MakcuClient,
    run: RunControl,
    // Must be dropped after all DXGI/DirectML objects owned by this worker.
    graphics: Option<GraphicsThread>,
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

pub(super) fn run_worker(links: WorkerLinks, config: Config) {
    tracing::info!("engine worker started");
    let WorkerLinks {
        commands,
        events,
        state,
        ui_ctx,
        wait,
        qpc,
        telemetry,
    } = links;
    let mut status = StatusHub::new(state, ui_ctx.clone());
    let graphics = match GraphicsThread::initialize() {
        Ok(apartment) => Some(apartment),
        Err(error) => {
            tracing::error!(error = %crate::logging::chain(&error), "graphics thread COM initialization failed");
            let message = format!("Graphics thread COM initialization failed: {error:#}");
            status.edit(|status| {
                status.capture.phase = CapturePhase::Failed;
                status.capture.error = Some(message.clone());
                status.graphics.catalog_error = Some(message);
            });
            status.flush();
            None
        }
    };

    let tuning = Tuning {
        output: OutputSettings {
            sensitivity: config.sensitivity,
            target_y_offset: config.target_y_offset,
            max_delta: config.max_delta,
        },
        confidence_threshold: config.confidence_threshold,
        fps_limit: config.fps_limit,
        capture: config.capture,
        low_latency: config.low_latency,
        frame_trace: config.diagnostics.frame_trace,
    };
    let mut makcu = MakcuClient::new();
    makcu.set_port(config.makcu_port.clone());
    // Before the first connect, so this sends nothing: the value is sent when the connect succeeds.
    if let Err(error) = makcu.set_interpolation(config.interpolation) {
        tracing::warn!(error = %crate::logging::chain(&error), "move interpolation could not be applied");
    }
    let _boost = crate::platform::boost_current_thread("engine");
    let mut worker = Worker {
        event_tx: events,
        ui_ctx,
        status,
        qpc,
        telemetry,
        tuning,
        displays: Vec::new(),
        capture: CaptureState::None,
        output_stage: OutputStage::new(tuning.output),
        wait,
        capture_deadline: frame::CaptureDeadline::new(),
        run_counters: RunCounters::default(),
        sent_counters: RunCounters::default(),
        active_model: None,
        auto_reprepare_attempted: false,
        next_pipeline_id: 1,
        capture_adapter: 0,
        capture_output: config.capture_output,
        selected_adapter_luid: config.selected_adapter,
        makcu,
        run: RunControl::default(),
        graphics,
    };

    worker.refresh_capture_catalog();
    worker.initialize_capture();
    worker.connect_makcu_initial();
    if let Some(name) = config.model {
        match models::resolve_model(&name) {
            Ok(path) => worker.load_model(false, name, path),
            Err(error) => {
                tracing::error!(model = %name, error = %crate::logging::chain(&error), "configured model could not be resolved");
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
    /// Periodic work that no event triggers: MAKCU connection changes, and the Run counters
    /// that the Telemetry thread shows. Each value is sent only when it changed.
    fn housekeeping(&mut self) {
        self.poll_makcu_events();
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
                match self.capture_deadline.pending_until(run.mode, now) {
                    Some(at) => capture_at = Some(at),
                    None => {
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
            self.wait.wait_until(Some(deadline), spin);
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
                self.tuning.output.sensitivity = value;
                self.push_output_settings();
            }
            Command::SetTargetYOffset(value) => {
                self.tuning.output.target_y_offset = value;
                self.push_output_settings();
            }
            Command::SetMaxDelta(value) => {
                self.tuning.output.max_delta = value;
                self.push_output_settings();
            }
            Command::SetConfidenceThreshold(value) => {
                self.tuning.confidence_threshold = value;
                if let Some(pipeline) = self.capture.pipeline_mut() {
                    pipeline.set_confidence_threshold(value);
                }
            }
            Command::SetInterpolation(interpolation) => {
                if let Err(error) = self.makcu.set_interpolation(interpolation) {
                    tracing::warn!(error = %crate::logging::chain(&error), "move interpolation failed");
                    self.telemetry.event(TraceEvent::MakcuInterpolationFailed {
                        reason: format!("{error:#}"),
                    });
                    self.set_run_error(format!("{error:#}"));
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
            Command::SetCaptureSettings(settings) => {
                self.tuning.capture = settings;
            }
            Command::SetFrameTrace(enabled) => self.tuning.frame_trace = enabled,
            Command::SetCaptureSelection { adapter, output } => {
                self.auto_reprepare_attempted = false;
                self.set_capture_selection(adapter, output)
            }
            Command::SetContinuous(active) => self.set_continuous(active),
            Command::RunOnce => self.run_once(),
            Command::RefreshMakcuRoutes => {
                self.reconnect_makcu("refreshing", MakcuClient::refresh_routes)
            }
            Command::RescanMakcu => self.reconnect_makcu("rescanning", MakcuClient::rescan),
            Command::SetMakcuPort(port) => {
                self.makcu.set_port(port);
                self.reconnect_makcu("selecting the port", MakcuClient::rescan)
            }
            Command::RetryCapture => {
                self.auto_reprepare_attempted = false;
                self.retry_suspended_pipeline();
            }
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
        self.output_stage.set_settings(self.tuning.output);
    }

    fn set_run_error(&mut self, error: String) {
        self.status.edit(|status| status.run.error = Some(error));
    }

    fn emit_event(&self, event: EngineEvent) {
        let _ = self.event_tx.send(event);
        repaint::request_repaint(&self.ui_ctx);
    }

    fn next_pipeline_id(&mut self) -> u64 {
        let id = self.next_pipeline_id;
        self.next_pipeline_id = id.wrapping_add(1).max(1);
        id
    }
}
