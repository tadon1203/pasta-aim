//! The engine owns and executes the complete serial frame pipeline.

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

use crossbeam_channel::{Receiver, RecvTimeoutError, TryRecvError};
use eframe::egui;

use self::run::RunControl;
use super::{
    CapturePhase, Command, EngineEvent, EngineStatus,
    loop_metrics::LoopMetrics,
    output::{OutputSettings, OutputStage},
    telemetry::{RunSummary, StatusHub},
};
use crate::{
    config::Config,
    graphics::{DxgiCapture, GraphicsThread},
    inference::AdapterLuid,
    makcu::MakcuClient,
    metrics::Metrics,
    models,
    settings::{CaptureSettings, ConfidenceThreshold, FpsLimit},
    timing::QpcClock,
};

/// How often the engine polls MAKCU connection state when no other event wakes it.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(100);

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

struct Worker {
    event_tx: Sender<EngineEvent>,
    ui_ctx: egui::Context,
    status: StatusHub,
    qpc: QpcClock,
    tuning: Tuning,

    // Pipeline (replaced as a unit when the display or GPU changes).
    model: Option<capture::PreparedModel>,
    capture: Option<DxgiCapture>,
    output_stage: OutputStage,
    trace: Option<super::frame_trace::FrameTraceWriter>,
    next_capture: Instant,
    frame_seq: u64,
    source_seq: u64,
    capture_stats: frame::CaptureStats,
    capture_stats_started: Instant,
    active_model: Option<(String, PathBuf)>,
    model_ready: bool,
    auto_reprepare_attempted: bool,
    next_pipeline_id: u64,
    active_pipeline_id: Option<u64>,
    capture_adapter: u32,
    capture_output: u32,
    selected_adapter_luid: AdapterLuid,

    makcu: MakcuClient,
    run: RunControl,
    summary: RunSummary,
    metrics: Metrics,
    loop_metrics: LoopMetrics,
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
            max_delta: config.max_delta,
        },
        confidence_threshold: config.confidence_threshold,
        fps_limit: config.fps_limit,
        capture: config.capture,
        low_latency: config.low_latency,
        frame_trace: config.diagnostics.frame_trace,
    };
    let makcu = MakcuClient::new();
    let _boost = crate::platform::boost_current_thread("engine");
    let mut worker = Worker {
        event_tx,
        ui_ctx,
        status,
        qpc,
        tuning,
        model: None,
        capture: None,
        output_stage: OutputStage::new(tuning.output),
        trace: None,
        next_capture: Instant::now(),
        frame_seq: 0,
        source_seq: 0,
        capture_stats: frame::CaptureStats::default(),
        capture_stats_started: Instant::now(),
        active_model: None,
        model_ready: false,
        auto_reprepare_attempted: false,
        next_pipeline_id: 1,
        active_pipeline_id: None,
        capture_adapter: 0,
        capture_output: config.capture_output,
        selected_adapter_luid: config.selected_adapter,
        makcu,
        run: RunControl::default(),
        summary: RunSummary::new(),
        metrics: Metrics::new(),
        loop_metrics: LoopMetrics::new(),
        graphics,
    };

    worker.refresh_capture_catalog();
    worker.initialize_capture();
    if let Err(error) = worker.makcu.set_injection_api(config.injection_api) {
        tracing::error!(error = %format!("{error:#}"), "initial injection API setup failed");
    }
    worker.connect_makcu_initial();
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
    /// Periodic work that no event triggers: MAKCU connection changes and
    /// folding capture-side counters into metrics.
    fn housekeeping(&mut self) {
        self.poll_makcu_events();
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
            self.publish_capture_stats(false);
            self.status.flush_if_due();
            if let Some(run) = self.run.active
                && frame::capture_due(run.mode, now, self.next_capture)
            {
                if !self.step_frame(rx) {
                    return;
                }
                continue;
            }
            let mut timeout = next_housekeeping.saturating_duration_since(Instant::now());
            if self.run.active.is_some() {
                timeout = timeout.min(self.next_capture.saturating_duration_since(Instant::now()));
            }
            if let Some(until_flush) = self.status.time_until_flush() {
                timeout = timeout.min(until_flush);
            }
            match rx.recv_timeout(timeout) {
                Ok(command) => {
                    if !self.handle_command(command) {
                        return;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.stop_run();
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
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
                if let Some(model) = self.model.as_mut() {
                    model.pipeline.set_confidence_threshold(value);
                }
            }
            Command::SetFpsLimit(value) => {
                self.tuning.fps_limit = value;
                self.next_capture = Instant::now();
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
            Command::SetInjectionApi(api) => {
                let result = self.makcu.set_injection_api(api);
                match result {
                    Ok(()) => {}
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
            Command::RefreshMakcuRoutes => {
                self.reconnect_makcu("refreshing", MakcuClient::refresh_routes)
            }
            Command::RescanMakcu => self.reconnect_makcu("rescanning", MakcuClient::rescan),
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
        self.ui_ctx.request_repaint();
    }

    fn next_pipeline_id(&mut self) -> u64 {
        let id = self.next_pipeline_id;
        self.next_pipeline_id = id.wrapping_add(1).max(1);
        id
    }
}
