//! The engine thread: owns the capture/inference workers and the MAK client,
//! applies UI commands, and turns worker events into UI-facing status.
//!
//! The per-frame path (inference → output) runs on the inference thread; this
//! thread only aggregates telemetry and reacts to control events.

mod capture;
mod mak;
mod model;
mod recovery;
mod run;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, mpsc::Sender},
    time::{Duration, Instant},
};

use anyhow::Result;
use crossbeam_channel::{Receiver, select};
use eframe::egui;

use self::run::RunControl;
use super::{
    CapturePhase, Command, EngineEvent, EngineStatus,
    capture_worker::CaptureWorker,
    inference_worker::{InferenceConfig, InferenceWorker},
    output::{MotionGate, OutputSettings},
    telemetry::{RunSummary, StatusHub},
};
use crate::{
    config::Config,
    graphics::{FrameConsumer, GraphicsThread},
    inference::AdapterLuid,
    mak::MakClient,
    metrics::Metrics,
    models,
    settings::{ConfidenceThreshold, FpsLimit, OutputMode},
    sync::MutexExt,
    timing::QpcClock,
};

const PHYSICAL_CONTROLLER_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// How often the engine polls MAK connection state when no other event wakes it.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(100);

/// Values the engine forwards to the worker threads it spawns.
#[derive(Clone, Copy)]
struct Tuning {
    output: OutputSettings,
    confidence_threshold: ConfidenceThreshold,
    fps_limit: FpsLimit,
}

struct Worker {
    event_tx: Sender<EngineEvent>,
    ui_ctx: egui::Context,
    status: StatusHub,
    qpc: QpcClock,
    tuning: Tuning,

    // Pipeline (replaced as a unit when the display or GPU changes).
    capture_worker: CaptureWorker,
    inference_worker: InferenceWorker,
    frame_consumer: FrameConsumer,
    active_model: Option<(String, PathBuf)>,
    model_ready: bool,
    auto_reprepare_attempted: bool,
    next_pipeline_id: u64,
    active_pipeline_id: Option<u64>,
    capture_adapter: u32,
    capture_output: u32,
    selected_adapter_luid: AdapterLuid,

    mak: Arc<Mutex<MakClient>>,
    gate: MotionGate,
    run: RunControl,
    summary: RunSummary,
    metrics: Metrics,
    last_physical_probe: Instant,
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

/// Spawns the capture and inference threads sharing one frame mailbox.
fn spawn_workers(
    qpc: QpcClock,
    tuning: Tuning,
    mak: &Arc<Mutex<MakClient>>,
    gate: &MotionGate,
) -> Result<(CaptureWorker, InferenceWorker, FrameConsumer)> {
    let (capture, consumer) = CaptureWorker::spawn(qpc, gate.clone())?;
    capture.set_fps_limit(tuning.fps_limit)?;
    let inference =
        InferenceWorker::spawn(qpc, consumer.clone(), inference_config(tuning, mak, gate))?;
    Ok((capture, inference, consumer))
}

fn inference_config(
    tuning: Tuning,
    mak: &Arc<Mutex<MakClient>>,
    gate: &MotionGate,
) -> InferenceConfig {
    InferenceConfig {
        confidence_threshold: tuning.confidence_threshold,
        output: tuning.output,
        mak: Arc::clone(mak),
        gate: gate.clone(),
    }
}

pub(super) fn run_worker(
    rx: Receiver<Command>,
    event_tx: Sender<EngineEvent>,
    shared_status: Arc<Mutex<EngineStatus>>,
    config: Config,
    ui_ctx: egui::Context,
) {
    tracing::info!("engine worker started");
    let mut status = StatusHub::new(shared_status, ui_ctx.clone());
    let graphics = match GraphicsThread::initialize() {
        Ok(apartment) => Some(apartment),
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "graphics thread COM initialization failed");
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

    let qpc = match QpcClock::new() {
        Ok(qpc) => qpc,
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "QPC initialization failed");
            return;
        }
    };
    let tuning = Tuning {
        output: OutputSettings {
            sensitivity: config.sensitivity,
            target_y_offset: config.target_y_offset,
            mode: config.output_mode,
        },
        confidence_threshold: config.confidence_threshold,
        fps_limit: config.fps_limit,
    };
    let mak = Arc::new(Mutex::new(MakClient::new()));
    let gate = MotionGate::new();
    let (capture_worker, inference_worker, frame_consumer) =
        match spawn_workers(qpc, tuning, &mak, &gate) {
            Ok(workers) => workers,
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "worker initialization failed");
                return;
            }
        };
    let mut worker = Worker {
        event_tx,
        ui_ctx,
        status,
        qpc,
        tuning,
        capture_worker,
        inference_worker,
        frame_consumer,
        active_model: None,
        model_ready: false,
        auto_reprepare_attempted: false,
        next_pipeline_id: 1,
        active_pipeline_id: None,
        capture_adapter: 0,
        capture_output: config.capture_output,
        selected_adapter_luid: config.selected_adapter,
        mak,
        gate,
        run: RunControl::default(),
        summary: RunSummary::new(),
        metrics: Metrics::new(),
        last_physical_probe: Instant::now() - PHYSICAL_CONTROLLER_POLL_INTERVAL,
        graphics,
    };

    worker.refresh_capture_catalog();
    worker.initialize_capture();
    if let Err(error) = worker.mak().set_injection_api(config.injection_api) {
        tracing::error!(error = %format!("{error:#}"), "initial injection API setup failed");
    }
    worker.connect_mak_initial();
    if let Some(name) = config.model {
        match models::resolve_model(&name) {
            Ok(path) => worker.load_model(false, name, path),
            Err(error) => {
                tracing::error!(model = %name, error = %format!("{error:#}"), "configured model could not be resolved");
                worker
                    .status
                    .edit(|status| status.model.error = Some(format!("{error:#}")));
            }
        }
    }
    worker.event_loop(&rx);
    worker.status.flush();
}

impl Worker {
    /// Periodic work that no event triggers: MAK connection changes, controller
    /// probing, and folding capture-side counters into metrics.
    fn housekeeping(&mut self) {
        self.poll_mak_events();
        if !self.run.is_continuous() {
            self.maybe_probe_physical_controller();
        }
        self.drain_gate_discards();
    }

    fn event_loop(&mut self, rx: &Receiver<Command>) {
        let mut next_housekeeping = Instant::now();
        loop {
            let now = Instant::now();
            if now >= next_housekeeping {
                self.housekeeping();
                next_housekeeping = now + HOUSEKEEPING_INTERVAL;
            }
            self.status.flush_if_due();
            let mut timeout = next_housekeeping.saturating_duration_since(Instant::now());
            if let Some(until_flush) = self.status.time_until_flush() {
                timeout = timeout.min(until_flush);
            }
            select! {
                recv(rx) -> command => match command {
                    Ok(command) => if !self.handle_command(command) { return; },
                    Err(_) => return,
                },
                recv(self.inference_worker.event_rx()) -> event => {
                    if let Ok(event) = event { self.handle_inference_event(event); }
                },
                recv(self.capture_worker.event_rx()) -> event => {
                    if let Ok(event) = event { self.handle_capture_event(event); }
                },
                default(timeout) => {},
            }
        }
    }

    fn mak(&self) -> MutexGuard<'_, MakClient> {
        self.mak.lock_recover()
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
            Command::SetConfidenceThreshold(value) => {
                self.tuning.confidence_threshold = value;
                self.inference_worker.set_confidence_threshold(value);
            }
            Command::SetFpsLimit(value) => {
                self.tuning.fps_limit = value;
                if let Err(error) = self.capture_worker.set_fps_limit(value) {
                    self.set_run_error(format!("Failed to set FPS limit: {error:#}"));
                }
            }
            Command::SetOutput(mode) => self.set_output_mode(mode),
            Command::SetInjectionApi(api) => {
                let result = self.mak().set_injection_api(api);
                match result {
                    Ok(()) => self.gate.reset(),
                    Err(error) => {
                        tracing::error!(
                            error = %format!("{error:#}"),
                            requested_api = ?api,
                            "injection API change failed"
                        );
                        self.set_run_error(format!("Injection API change failed: {error:#}"));
                    }
                }
            }
            Command::SetCaptureSelection { adapter, output } => {
                self.auto_reprepare_attempted = false;
                self.set_capture_selection(adapter, output)
            }
            Command::SetContinuous(active) => self.set_continuous(active),
            Command::RunOnce => self.run_once(),
            Command::RefreshMakRoutes => {
                self.reconnect_mak("refreshing", MakClient::refresh_routes)
            }
            Command::RescanMak => self.reconnect_mak("rescanning", MakClient::rescan),
            Command::RetryCapture => {
                self.auto_reprepare_attempted = false;
                self.retry_suspended_pipeline();
            }
            Command::ProbeController => self.probe_physical_controller(),
            Command::RescanDisplays => self.refresh_capture_catalog(),
            Command::Shutdown => {
                tracing::info!("engine worker shutdown requested");
                self.stop_run();
                return false;
            }
        }
        true
    }

    fn set_output_mode(&mut self, mode: OutputMode) {
        self.stop_run();
        if self.tuning.output.mode == OutputMode::Controller && mode != OutputMode::Controller {
            self.mak().release_controller_best_effort();
        }
        self.tuning.output.mode = mode;
        self.push_output_settings();
        if mode == OutputMode::Controller {
            self.last_physical_probe = Instant::now() - PHYSICAL_CONTROLLER_POLL_INTERVAL;
        }
        self.sync_release_pending_status();
        self.status.edit(|status| status.run.error = None);
    }

    fn spawn_inference(&self) -> Result<InferenceWorker> {
        InferenceWorker::spawn(
            self.qpc,
            self.frame_consumer.clone(),
            inference_config(self.tuning, &self.mak, &self.gate),
        )
    }

    fn push_output_settings(&self) {
        self.inference_worker.set_output(self.tuning.output);
    }

    fn set_run_error(&mut self, error: String) {
        self.status.edit(|status| status.run.error = Some(error));
    }

    fn emit_event(&self, event: EngineEvent) {
        let _ = self.event_tx.send(event);
        self.ui_ctx.request_repaint();
    }

    fn next_pipeline_id(&mut self) -> u64 {
        let id = self.next_pipeline_id;
        self.next_pipeline_id = id.wrapping_add(1).max(1);
        id
    }
}
