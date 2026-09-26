use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc::Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use crossbeam_channel::{Receiver, select};
use eframe::egui;

use super::status::{RunReadiness, run_block_reason};
use super::{
    CapturePhase, Command, EngineEvent, EngineStatus, MakPhase, ModelPhase, RunPhase,
    ep_assignment_summary,
};
use crate::{
    config::Config,
    engine::{
        RunGeneration,
        capture_worker::{CaptureEvent, CaptureFault, CaptureWorker},
        inference_worker::{InferenceControlEvent, InferenceFrameResult, InferenceWorker},
    },
    graphics::GraphicsThread,
    graphics::{FrameConsumer, GpuDropReason, GpuFaultReport},
    inference::AdapterLuid,
    mak::{MakClient, MakConnectionEvent, MakMetadata, MakRoutes, PhysicalControllerStatus},
    metrics::{Metrics, StageTimings},
    models,
    settings::{ConfidenceThreshold, FpsLimit, OutputMode, Sensitivity, TargetYOffset},
    timing::QpcClock,
};

const PHYSICAL_CONTROLLER_POLL_INTERVAL: Duration = Duration::from_secs(5);
const CONTROLLER_ONE_SHOT_PULSE: Duration = Duration::from_millis(16);
const CONTINUOUS_LOG_INTERVAL: Duration = Duration::from_secs(1);

/// The sole boundary between output calculation and MAK submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputCommand {
    MouseMove { x: i32, y: i32 },
    ControllerState { x: i32, y: i32 },
}

struct Worker {
    event_tx: Sender<EngineEvent>,
    status: Arc<Mutex<EngineStatus>>,
    ui_ctx: egui::Context,
    active_model: Option<(String, PathBuf)>,
    capture_worker: CaptureWorker,
    inference_worker: InferenceWorker,
    frame_consumer: FrameConsumer,
    model_ready: bool,
    auto_reprepare_attempted: bool,
    next_pipeline_id: u64,
    active_pipeline_id: Option<u64>,
    gpu_fault_history: VecDeque<GpuFaultReport>,
    capture_adapter: u32,
    capture_output: u32,
    selected_adapter_luid: AdapterLuid,
    mak: MakClient,
    metrics: Metrics,
    sensitivity: Sensitivity,
    confidence_threshold: ConfidenceThreshold,
    target_y_offset: TargetYOffset,
    fps_limit: FpsLimit,
    output_mode: OutputMode,
    continuous: bool,
    run_generation: RunGeneration,
    active_generation: Option<RunGeneration>,
    last_motion_complete_qpc: Option<i64>,
    qpc: QpcClock,
    last_physical_probe: Instant,
    log_summary_started: Instant,
    log_summary_frames: usize,
    log_summary_detections: usize,
    log_summary_outputs: usize,
    log_summary_last_command: Option<(i32, i32, OutputMode)>,
    // Must be dropped after all DXGI/DirectML objects owned by this worker.
    graphics: Option<GraphicsThread>,
}

pub(super) fn run_worker(
    rx: Receiver<Command>,
    event_tx: Sender<EngineEvent>,
    status: Arc<Mutex<EngineStatus>>,
    config: Config,
    ui_ctx: egui::Context,
) {
    tracing::info!("engine worker started");
    let graphics = match GraphicsThread::initialize() {
        Ok(apartment) => Some(apartment),
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "graphics thread COM initialization failed");
            {
                let mut current = status
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let message = format!("Graphics thread COM initialization failed: {error:#}");
                current.capture.phase = CapturePhase::Failed;
                current.capture.error = Some(message.clone());
                current.graphics.catalog_error = Some(message);
            }
            ui_ctx.request_repaint();
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
    let (capture_worker, frame_consumer) = match CaptureWorker::spawn(qpc).and_then(
        |(worker, consumer)| {
            worker.set_fps_limit(config.fps_limit)?;
            Ok((worker, consumer))
        },
    ) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "capture worker initialization failed");
            return;
        }
    };
    let inference_worker = match InferenceWorker::spawn(
        qpc,
        frame_consumer.clone(),
        config.confidence_threshold,
    ) {
        Ok(worker) => worker,
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "inference worker initialization failed");
            return;
        }
    };
    let mut worker = Worker {
        event_tx,
        status,
        ui_ctx,
        active_model: None,
        capture_worker,
        inference_worker,
        frame_consumer,
        model_ready: false,
        auto_reprepare_attempted: false,
        next_pipeline_id: 1,
        active_pipeline_id: None,
        gpu_fault_history: VecDeque::with_capacity(2),
        capture_adapter: 0,
        capture_output: config.capture_output,
        selected_adapter_luid: config.selected_adapter,
        mak: MakClient::new(),
        metrics: Metrics::new(),
        sensitivity: config.sensitivity,
        confidence_threshold: config.confidence_threshold,
        target_y_offset: config.target_y_offset,
        fps_limit: config.fps_limit,
        output_mode: config.output_mode,
        continuous: false,
        run_generation: 0,
        active_generation: None,
        last_motion_complete_qpc: None,
        qpc,
        last_physical_probe: Instant::now() - PHYSICAL_CONTROLLER_POLL_INTERVAL,
        log_summary_started: Instant::now(),
        log_summary_frames: 0,
        log_summary_detections: 0,
        log_summary_outputs: 0,
        log_summary_last_command: None,
        graphics,
    };

    worker.refresh_capture_catalog();
    worker.initialize_capture();
    if let Err(error) = worker.mak.set_injection_api(config.injection_api) {
        tracing::error!(error = %format!("{error:#}"), "initial injection API setup failed");
    }
    worker.connect_mak_initial();
    if let Some(name) = config.model {
        match models::resolve_model(&name) {
            Ok(path) => worker.load_model(false, name, path),
            Err(error) => worker.edit_status(|status| {
                tracing::error!(model = %name, error = %format!("{error:#}"), "configured model could not be resolved");
                status.model.error = Some(format!("{error:#}"));
            }),
        }
    }

    loop {
        worker.poll_mak_events();
        if !worker.continuous {
            worker.maybe_probe_physical_controller();
        }
        while let Ok(command) = rx.try_recv() {
            if !worker.handle_command(command) {
                return;
            }
        }
        select! {
            recv(rx) -> command => match command {
                Ok(command) => if !worker.handle_command(command) { return; },
                Err(_) => return,
            },
            recv(worker.inference_worker.result_ready_rx()) -> _ => {
                if let Some(result) = worker.inference_worker.take_latest_result()
                    && let Err(error) = worker.handle_completed_inference(result)
                {
                    worker.stop_after_run_error(format!("Inference output failed: {error:#}"));
                }
                let superseded = worker.inference_worker.take_result_superseded();
                for _ in 0..superseded {
                    worker.metrics.record_gpu_drop(GpuDropReason::ResultSuperseded);
                }
            },
            recv(worker.inference_worker.event_rx()) -> event => {
                if let Ok(event) = event { worker.handle_inference_event(event); }
            },
            recv(worker.capture_worker.event_rx()) -> event => {
                if let Ok(event) = event { worker.handle_capture_event(event); }
            },
        }
    }
}

impl Worker {
    fn handle_command(&mut self, command: Command) -> bool {
        tracing::debug!(?command, "engine command received");
        match command {
            Command::LoadModel { name, path, notify } => self.load_model(notify, name, path),
            Command::SetSensitivity(value) => self.sensitivity = value,
            Command::SetConfidenceThreshold(value) => {
                self.confidence_threshold = value;
                self.inference_worker.set_confidence_threshold(value)
            }
            Command::SetTargetYOffset(value) => self.target_y_offset = value,
            Command::SetFpsLimit(value) => {
                self.fps_limit = value;
                if let Err(error) = self.capture_worker.set_fps_limit(value) {
                    self.edit_status(|status| {
                        status.run.error = Some(format!("Failed to set FPS limit: {error:#}"))
                    });
                }
            }
            Command::SetOutput(mode) => {
                self.stop_continuous();
                if self.output_mode == OutputMode::Controller && mode != OutputMode::Controller {
                    self.mak.release_controller_best_effort();
                }
                self.output_mode = mode;
                if mode == OutputMode::Controller {
                    self.last_physical_probe = Instant::now() - PHYSICAL_CONTROLLER_POLL_INTERVAL;
                }
                self.sync_release_pending_status();
                self.clear_run_error();
            }
            Command::SetInjectionApi(api) => match self.mak.set_injection_api(api) {
                Ok(()) => self.last_motion_complete_qpc = None,
                Err(error) => {
                    tracing::error!(
                        error = %format!("{error:#}"),
                        requested_api = ?api,
                        "injection API change failed"
                    );
                    self.edit_status(|status| {
                        status.run.error = Some(format!("Injection API change failed: {error:#}"));
                    });
                }
            },
            Command::SetCaptureSelection { adapter, output } => {
                self.auto_reprepare_attempted = false;
                self.set_capture_selection(adapter, output)
            }
            Command::SetContinuous(active) => self.set_continuous(active),
            Command::RunOnce => self.run_once(),
            Command::RefreshMakRoutes => self.refresh_mak_routes(),
            Command::RescanMak => self.rescan_mak(),
            Command::RetryCapture => {
                self.auto_reprepare_attempted = false;
                self.retry_suspended_pipeline();
            }
            Command::ProbeController => self.probe_physical_controller(),
            Command::RescanDisplays => self.refresh_capture_catalog(),
            Command::Shutdown => {
                tracing::info!("engine worker shutdown requested");
                self.stop_continuous();
                self.mak.release_controller_best_effort();
                self.sync_release_pending_status();
                return false;
            }
        }
        true
    }

    fn refresh_capture_catalog(&mut self) {
        let Some(graphics) = self.graphics.as_ref() else {
            return;
        };

        let displays = graphics.enumerate_displays();
        match displays {
            Ok(displays) => self.edit_status(|status| {
                status.graphics.displays = displays;
                status.graphics.catalog_error = None;
            }),
            Err(error) => {
                let error = format!("{error:#}");
                self.edit_status(|status| {
                    tracing::error!(%error, "graphics enumeration failed");
                    status.graphics.displays.clear();
                    status.graphics.catalog_error = Some(error);
                });
            }
        }
    }

    fn initialize_capture(&mut self) {
        if let Some(display) = self.status.lock().ok().and_then(|status| {
            status
                .graphics
                .displays
                .iter()
                .find(|display| {
                    display.adapter_luid == self.selected_adapter_luid
                        && display.output_index == self.capture_output
                })
                .cloned()
        }) {
            self.capture_adapter = display.adapter_index;
        }
        tracing::info!(
            adapter = self.capture_adapter,
            output = self.capture_output,
            "initializing selected capture"
        );
        self.edit_status(|status| {
            status.capture.phase = CapturePhase::Initializing;
            status.capture.error = None;
            status.capture.desktop_size = None;
        });

        match self
            .capture_worker
            .configure_output(self.capture_adapter, self.capture_output)
        {
            Ok(selected) => {
                self.selected_adapter_luid = selected.adapter_luid;
                self.edit_status(|status| {
                    status.capture.phase = CapturePhase::Ready;
                    status.capture.selected = Some(selected);
                    status.capture.error = None;
                });
                tracing::info!("selected capture initialized");
            }
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "selected capture initialization failed");
                self.edit_status(|status| {
                    status.capture.phase = CapturePhase::Failed;
                    status.capture.selected = None;
                    status.capture.error = Some(format!("{error:#}"));
                });
            }
        }
    }

    fn set_capture_selection(&mut self, adapter: u32, output: u32) {
        tracing::info!(adapter, output, "changing capture selection");
        let current = self
            .status
            .lock()
            .ok()
            .and_then(|status| status.capture.selected.clone());
        if let Some(selected) = &current
            && selected.adapter_index == adapter
            && selected.output_index == output
        {
            self.emit_event(EngineEvent::Capture {
                adapter,
                output,
                adapter_luid: selected.adapter_luid,
                result: Ok(()),
            });
            return;
        }
        let was_continuous = self.continuous;
        let prepared = self.prepare_pipeline(adapter, output);
        let (capture, inference, consumer, selected, loaded) = match prepared {
            Ok(value) => value,
            Err(error) => {
                let message = format!("Display change failed: {error:#}");
                self.edit_status(|status| {
                    status.capture.phase = if status.capture.selected.is_some() {
                        CapturePhase::Ready
                    } else {
                        CapturePhase::Failed
                    };
                    status.capture.error = Some(message.clone());
                });
                self.emit_event(EngineEvent::Capture {
                    adapter,
                    output,
                    adapter_luid: current
                        .map(|display| display.adapter_luid)
                        .unwrap_or(self.selected_adapter_luid),
                    result: Err(message.clone()),
                });
                tracing::error!(adapter, output, %message, "display prepare failed; active pipeline retained");
                return;
            }
        };

        let was_running = self.active_generation.take().is_some();
        if was_running {
            self.mak.release_controller_best_effort();
            let _ = self.inference_worker.stop(self.run_generation);
            let _ = self.capture_worker.stop();
        }
        self.continuous = false;
        let generation = self.next_generation();
        self.capture_adapter = adapter;
        self.capture_output = output;
        self.selected_adapter_luid = selected.adapter_luid;
        self.capture_worker = capture;
        self.inference_worker = inference;
        self.frame_consumer = consumer;
        self.metrics.mark_cold_next();
        self.model_ready = loaded.is_some();
        self.active_pipeline_id = None;
        if let Some(info) = loaded {
            self.active_pipeline_id = Some(info.pipeline_id);
            self.gpu_pipeline_state(
                "Running",
                Some(info.pipeline_id),
                Some(&info.prepare_id),
                None,
                None,
            );
            tracing::info!(backend = %info.backend_info.name, luid_low = info.backend_info.adapter_luid.low, luid_high = info.backend_info.adapter_luid.high, "prepared inference backend committed");
            let assignment_summary = ep_assignment_summary(&info.ep_assignment);
            self.edit_status(|status| {
                status.model.phase = ModelPhase::Ready;
                status.model.input_size = Some(info.input_size);
                status.model.precision = Some(info.precision);
                status.model.error = None;
                status.model.ep_assignment = Some(info.ep_assignment);
                status.model.ep_assignment_summary = assignment_summary;
            });
        }
        self.edit_status(|status| {
            status.capture.phase = CapturePhase::Ready;
            status.capture.selected = Some(selected.clone());
            status.capture.desktop_size = None;
            status.capture.error = None;
            status.run.error = None;
            status.run.phase = RunPhase::Idle;
        });
        self.emit_event(EngineEvent::Capture {
            adapter,
            output,
            adapter_luid: selected.adapter_luid,
            result: Ok(()),
        });
        if was_continuous && self.model_ready {
            self.last_motion_complete_qpc = None;
            if let Err(error) = self
                .capture_worker
                .start_continuous(generation)
                .and_then(|()| self.inference_worker.start(generation))
            {
                let _ = self.capture_worker.stop();
                let _ = self.inference_worker.stop(generation);
                self.edit_status(|status| {
                    status.run.error =
                        Some(format!("Pipeline committed but restart failed: {error:#}"))
                });
            } else {
                self.active_generation = Some(generation);
                self.continuous = true;
                self.edit_status(|status| status.run.phase = RunPhase::Continuous);
            }
        }
        tracing::info!(
            adapter,
            output,
            luid_low = selected.adapter_luid.low,
            luid_high = selected.adapter_luid.high,
            "display pipeline committed"
        );
    }

    fn prepare_pipeline(
        &mut self,
        adapter: u32,
        output: u32,
    ) -> Result<(
        CaptureWorker,
        InferenceWorker,
        FrameConsumer,
        crate::graphics::DisplayInfo,
        Option<crate::engine::inference_worker::PreparedModelInfo>,
    )> {
        let pipeline_id = self.next_pipeline_id();
        self.gpu_pipeline_state("Preparing", Some(pipeline_id), None, None, None);
        let (capture, consumer) = CaptureWorker::spawn(self.qpc)?;
        capture.set_fps_limit(self.fps_limit)?;
        let selected = capture.configure_output(adapter, output)?;
        let inference =
            InferenceWorker::spawn(self.qpc, consumer.clone(), self.confidence_threshold)?;
        let loaded = if let Some((_, path)) = &self.active_model {
            let transport_device = capture.transport_device()?;
            let loaded = inference.load_model(
                path.clone(),
                selected.adapter_luid,
                transport_device,
                capture.publisher(),
                pipeline_id,
            )?;
            capture.configure_frame_pool(loaded.capture_frames.clone())?;
            Some(loaded)
        } else {
            None
        };
        Ok((capture, inference, consumer, selected, loaded))
    }

    fn load_model(&mut self, notify: bool, name: String, path: PathBuf) {
        self.auto_reprepare_attempted = false;
        let pipeline_id = self.next_pipeline_id();
        self.gpu_pipeline_state("Preparing", Some(pipeline_id), None, None, None);
        tracing::info!(model = %name, path = %path.display(), luid_low = self.selected_adapter_luid.low, luid_high = self.selected_adapter_luid.high, "loading model in engine");
        self.stop_continuous();
        self.mak.release_controller_best_effort();
        self.sync_release_pending_status();
        self.edit_status(|status| {
            status.model.phase = ModelPhase::Loading;
            status.model.loading_name = Some(name.clone());
            status.model.error = None;
            status.run.error = None;
        });

        let was_model_ready = self.model_ready;
        let result = (|| -> Result<(InferenceWorker, crate::engine::inference_worker::PreparedModelInfo)> {
            let candidate = InferenceWorker::spawn(self.qpc, self.frame_consumer.clone(), self.confidence_threshold)?;
            let transport_device = self.capture_worker.transport_device()?;
            let loaded = candidate.load_model(
                path.clone(),
                self.selected_adapter_luid,
                transport_device,
                self.capture_worker.publisher(),
                pipeline_id,
            )?;
            self.capture_worker.configure_frame_pool(loaded.capture_frames.clone())?;
            Ok((candidate, loaded))
        })();
        match result {
            Ok((candidate, loaded)) => {
                self.active_pipeline_id = Some(loaded.pipeline_id);
                self.gpu_pipeline_state(
                    "Running",
                    Some(loaded.pipeline_id),
                    Some(&loaded.prepare_id),
                    None,
                    None,
                );
                tracing::info!(backend = %loaded.backend_info.name, backend_kind = ?loaded.backend_info.kind, luid_low = loaded.backend_info.adapter_luid.low, luid_high = loaded.backend_info.adapter_luid.high, "prepared model backend committed");
                self.inference_worker = candidate;
                let size = loaded.input_size;
                let precision = loaded.precision;
                let assignment = loaded.ep_assignment;
                let assignment_summary = ep_assignment_summary(&assignment);
                self.model_ready = true;
                self.active_model = Some((name.clone(), path));
                self.metrics.mark_cold_next();
                self.edit_status(|status| {
                    status.model.phase = ModelPhase::Ready;
                    status.model.active_name = Some(name.clone());
                    status.model.loading_name = None;
                    status.model.input_size = Some(size);
                    status.model.precision = Some(precision);
                    status.model.error = None;
                    status.model.ep_assignment = Some(assignment);
                    status.model.ep_assignment_summary = assignment_summary;
                });
                if notify {
                    self.emit_event(EngineEvent::Model {
                        name: name.clone(),
                        result: Ok(()),
                    });
                }
                tracing::info!(model = %name, "model load succeeded");
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(model = %name, %message, "model load failed");
                let has_active_model = was_model_ready;
                self.model_ready = was_model_ready;
                self.edit_status(|status| {
                    status.model.phase = if has_active_model {
                        ModelPhase::Ready
                    } else {
                        ModelPhase::Empty
                    };
                    status.model.loading_name = None;
                    status.model.error = Some(message.clone());
                });
                if notify {
                    self.emit_event(EngineEvent::Model {
                        name,
                        result: Err(message),
                    });
                }
            }
        }
    }

    fn connect_mak_initial(&mut self) {
        tracing::info!("connecting MAK during engine startup");
        self.edit_status(|status| {
            status.mak.phase = MakPhase::Connecting;
            status.mak.error = None;
        });
        match self.mak.connect_initial() {
            Ok(metadata) => self.apply_mak_metadata(metadata),
            Err(error) => self.set_mak_failure(format!("{error:#}")),
        }
    }

    fn refresh_mak_routes(&mut self) {
        tracing::info!("refreshing MAK routes in engine");
        self.stop_continuous();
        self.mak.release_controller_best_effort();
        self.sync_release_pending_status();
        self.edit_status(|status| {
            status.mak.phase = MakPhase::Connecting;
            status.mak.routes = MakRoutes::default();
            status.mak.physical_controller = PhysicalControllerStatus::Unknown;
            status.mak.error = None;
        });

        match self.mak.refresh_routes() {
            Ok(metadata) => self.apply_mak_metadata(metadata),
            Err(error) => self.set_mak_failure(format!("{error:#}")),
        }
    }

    fn rescan_mak(&mut self) {
        tracing::info!("rescanning MAK in engine");
        self.stop_continuous();
        self.mak.release_controller_best_effort();
        self.sync_release_pending_status();
        self.edit_status(|status| {
            status.mak.phase = MakPhase::Connecting;
            status.mak.routes = MakRoutes::default();
            status.mak.physical_controller = PhysicalControllerStatus::Unknown;
            status.mak.error = None;
        });

        match self.mak.rescan() {
            Ok(metadata) => self.apply_mak_metadata(metadata),
            Err(error) => self.set_mak_failure(format!("{error:#}")),
        }
    }

    fn poll_mak_events(&mut self) {
        while let Some(event) = self.mak.poll_connection_event() {
            match event {
                MakConnectionEvent::Disconnected | MakConnectionEvent::Connecting => {
                    tracing::warn!(?event, "MAK connection state changed");
                    self.stop_continuous();
                    let pending = self.mak.controller_release_pending();
                    self.edit_status(|status| {
                        status.mak.phase = MakPhase::Reconnecting;
                        status.mak.routes = MakRoutes::default();
                        status.mak.physical_controller = PhysicalControllerStatus::Unknown;
                        status.mak.controller_release_pending = pending;
                        status.mak.error = None;
                    });
                }
                MakConnectionEvent::Connected(Ok(metadata)) => self.apply_mak_metadata(metadata),
                MakConnectionEvent::Connected(Err(error)) => self.set_mak_failure(error),
            }
        }
    }

    fn apply_mak_metadata(&mut self, metadata: MakMetadata) {
        tracing::info!(port = %metadata.port, firmware = metadata.firmware, "MAK connected in engine");
        let physical = if metadata.routes.controller_available() {
            PhysicalControllerStatus::Checking
        } else {
            PhysicalControllerStatus::NotRouted
        };
        self.last_physical_probe = Instant::now() - PHYSICAL_CONTROLLER_POLL_INTERVAL;
        self.edit_status(|status| {
            status.mak.phase = MakPhase::Connected;
            status.mak.port = Some(metadata.port);
            status.mak.firmware = Some(metadata.firmware);
            status.mak.routes = metadata.routes;
            status.mak.physical_controller = physical;
            status.mak.controller_release_pending = metadata.controller_release_pending;
            status.mak.error = None;
        });
    }

    fn set_mak_failure(&mut self, error: String) {
        tracing::error!(%error, "MAK operation failed");
        self.stop_continuous();
        let pending = self.mak.controller_release_pending();
        self.edit_status(|status| {
            status.mak.phase = MakPhase::Failed;
            status.mak.routes = MakRoutes::default();
            status.mak.physical_controller = PhysicalControllerStatus::Unknown;
            status.mak.controller_release_pending = pending;
            status.mak.error = Some(error);
        });
    }

    fn sync_release_pending_status(&self) {
        let pending = self.mak.controller_release_pending();
        self.edit_status(|status| status.mak.controller_release_pending = pending);
    }

    fn maybe_probe_physical_controller(&mut self) {
        if self.last_physical_probe.elapsed() < PHYSICAL_CONTROLLER_POLL_INTERVAL {
            return;
        }
        if !self.mak.is_connected() || !self.mak.controller_available() {
            return;
        }

        let should_probe = self.output_mode == OutputMode::Controller
            || self.mak.physical_controller_status() == PhysicalControllerStatus::Checking;
        if should_probe {
            self.probe_physical_controller();
        }
    }

    fn probe_physical_controller(&mut self) {
        if self.continuous || !self.mak.is_connected() {
            return;
        }
        self.last_physical_probe = Instant::now();
        self.edit_status(|status| {
            if status.mak.routes.controller_available() {
                status.mak.physical_controller = PhysicalControllerStatus::Checking;
            }
        });
        let physical = self.mak.probe_physical_controller();
        tracing::debug!(?physical, "physical controller probe completed");
        self.edit_status(|status| status.mak.physical_controller = physical);
    }

    fn set_continuous(&mut self, active: bool) {
        self.set_continuous_inner(active);
    }

    fn set_continuous_inner(&mut self, active: bool) {
        if !active {
            self.stop_continuous();
            return;
        }
        if self.continuous {
            return;
        }
        if let Some(reason) = self.run_block_reason() {
            tracing::warn!(%reason, "continuous inference blocked");
            self.edit_status(|status| status.run.error = Some(reason));
            return;
        }

        let generation = self.next_generation();
        self.last_motion_complete_qpc = None;
        if let Err(error) = self
            .capture_worker
            .start_continuous(generation)
            .and_then(|()| self.inference_worker.start(generation))
        {
            let _ = self.capture_worker.stop();
            let _ = self.inference_worker.stop(generation);
            self.active_generation = None;
            self.edit_status(|status| {
                status.run.error = Some(format!("Failed to start inference: {error:#}"))
            });
            return;
        }
        self.active_generation = Some(generation);
        self.continuous = true;
        self.reset_log_summary();
        tracing::info!(
            fps_limit = self.fps_limit.value(),
            "continuous inference started"
        );
        self.edit_status(|status| {
            status.run.phase = RunPhase::Continuous;
            status.run.error = None;
        });
    }

    fn run_once(&mut self) {
        if self.continuous {
            tracing::warn!("single inference requested while continuous inference is active");
            self.edit_status(|status| {
                status.run.error = Some("Stop continuous inference before running once".to_owned());
            });
            return;
        }
        if let Some(reason) = self.run_block_reason() {
            tracing::warn!(%reason, "single inference blocked");
            self.edit_status(|status| status.run.error = Some(reason));
            return;
        }

        let generation = self.next_generation();
        self.last_motion_complete_qpc = None;
        self.edit_status(|status| {
            status.run.phase = RunPhase::OneShot;
            status.run.error = None;
        });
        if let Err(error) = self
            .capture_worker
            .capture_once(generation)
            .and_then(|()| self.inference_worker.start(generation))
        {
            self.active_generation = None;
            let _ = self.capture_worker.stop();
            let _ = self.inference_worker.stop(generation);
            tracing::error!(error = %format!("{error:#}"), "single inference failed to start");
            self.mak.release_controller_best_effort();
            self.edit_status(|status| {
                status.run.phase = RunPhase::Idle;
                status.run.error = Some(format!("Single inference failed: {error:#}"));
            });
        } else {
            self.active_generation = Some(generation);
        }
    }

    fn stop_continuous(&mut self) {
        let old_generation = self.active_generation.take();
        self.continuous = false;
        if let Some(generation) = old_generation {
            tracing::info!(generation, "inference stopped");
            self.mak.release_controller_best_effort();
            let _ = self.inference_worker.stop(generation);
            let _ = self.capture_worker.stop();
        }
        let pending = self.mak.controller_release_pending();
        self.edit_status(|status| {
            if status.run.phase != RunPhase::Idle {
                status.run.phase = RunPhase::Idle;
            }
            status.mak.controller_release_pending = pending;
        });
    }

    fn stop_after_run_error(&mut self, error: String) {
        tracing::error!(%error, "continuous inference stopped after error");
        let generation = self.active_generation.take();
        self.continuous = false;
        if let Some(generation) = generation {
            let _ = self.inference_worker.stop(generation);
            let _ = self.capture_worker.stop();
        }
        self.mak.release_controller_best_effort();
        let pending = self.mak.controller_release_pending();
        self.edit_status(|status| {
            status.run.phase = RunPhase::Idle;
            status.run.error = Some(error);
            status.mak.controller_release_pending = pending;
        });
    }

    fn suspend_invalid_pipeline(&mut self, error: String) {
        self.stop_after_run_error(error.clone());
        self.continuous = false;
        self.model_ready = false;
        self.active_generation = None;
        self.edit_status(|status| {
            status.capture.phase = CapturePhase::Failed;
            status.capture.selected = None;
            status.capture.desktop_size = None;
            status.capture.error = Some(error.clone());
        });

        // Stop and join both owners before constructing replacement devices.
        self.inference_worker.shutdown();
        self.capture_worker.shutdown();
        let _ = self.persist_gpu_fault_reports(None);
        let replacement = (|| -> Result<(CaptureWorker, InferenceWorker, FrameConsumer)> {
            let (capture, consumer) = CaptureWorker::spawn(self.qpc)?;
            capture.set_fps_limit(self.fps_limit)?;
            let inference =
                InferenceWorker::spawn(self.qpc, consumer.clone(), self.confidence_threshold)?;
            Ok((capture, inference, consumer))
        })();
        match replacement {
            Ok((capture, inference, consumer)) => {
                drop(std::mem::replace(&mut self.inference_worker, inference));
                drop(std::mem::replace(&mut self.capture_worker, capture));
                self.frame_consumer = consumer;
            }
            Err(rebuild_error) => {
                tracing::error!(error = %format!("{rebuild_error:#}"), "failed to create suspended pipeline workers");
            }
        }
        tracing::error!(%error, "invalid GPU pipeline dropped; capture remains suspended");
    }

    fn retry_suspended_pipeline(&mut self) {
        self.stop_continuous();
        self.refresh_capture_catalog();
        let display = self.status.lock().ok().and_then(|status| {
            status
                .graphics
                .displays
                .iter()
                .find(|display| {
                    display.adapter_luid == self.selected_adapter_luid
                        && display.output_index == self.capture_output
                })
                .cloned()
        });
        if let Some(display) = display {
            self.set_capture_selection(display.adapter_index, display.output_index);
        } else {
            let error = "The previously selected capture adapter is unavailable".to_owned();
            self.edit_status(|status| {
                status.capture.phase = CapturePhase::Failed;
                status.capture.error = Some(error.clone());
                status.run.error = Some(error);
            });
        }
    }

    fn clear_run_error(&self) {
        self.edit_status(|status| status.run.error = None);
    }

    fn run_block_reason(&self) -> Option<String> {
        run_block_reason(
            RunReadiness {
                model_ready: self.model_ready,
                capture_ready: self
                    .status
                    .lock()
                    .map(|status| status.capture.phase == CapturePhase::Ready)
                    .unwrap_or(false),
                mak_connected: self.mak.is_connected(),
                mouse_route: self.mak.mouse_available(),
                controller_route: self.mak.controller_available(),
            },
            self.output_mode,
        )
        .map(str::to_owned)
    }

    fn next_generation(&mut self) -> RunGeneration {
        self.run_generation = self.run_generation.wrapping_add(1);
        if self.run_generation == 0 {
            self.run_generation = 1;
        }
        self.run_generation
    }

    fn next_pipeline_id(&mut self) -> u64 {
        let id = self.next_pipeline_id;
        self.next_pipeline_id = id.wrapping_add(1).max(1);
        id
    }

    fn gpu_pipeline_state(
        &self,
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

    fn handle_capture_fault(&mut self, fault: CaptureFault) {
        if let Some(generation) = fault.generation
            && self.active_generation != Some(generation)
            && !fault
                .report
                .as_ref()
                .is_some_and(|report| Some(report.pipeline_id) == self.active_pipeline_id)
        {
            tracing::debug!(generation, "discarding stale capture fault");
            return;
        }
        if fault.poisoned {
            self.recover_poisoned_pipeline(
                format!("Capture transport failed: {}", fault.error),
                fault.device_lost,
                fault.report,
            );
        } else {
            self.suspend_invalid_pipeline(format!("Capture failed: {}", fault.error));
        }
    }

    fn handle_capture_event(&mut self, event: CaptureEvent) {
        match event {
            CaptureEvent::Fault(fault) => self.handle_capture_fault(*fault),
            CaptureEvent::Stats(value) => self.metrics.update_capture_source(value),
            CaptureEvent::GpuCopy(sample) => {
                let _present_qpc = sample.present_qpc;
                self.metrics.record_gpu_copy(sample.gpu_copy_ms);
            }
        }
        if let Some(performance) = self.metrics.snapshot_if_due(false) {
            self.edit_status(|status| status.performance = performance);
        }
    }

    fn handle_inference_event(&mut self, event: InferenceControlEvent) {
        match event {
            InferenceControlEvent::Failed { generation, error } => {
                if self.active_generation == Some(generation) {
                    self.suspend_invalid_pipeline(error);
                }
            }
            InferenceControlEvent::Poisoned {
                generation,
                error,
                device_lost,
                report,
            } => {
                let belongs_to_active_pipeline = report
                    .as_ref()
                    .is_some_and(|report| Some(report.pipeline_id) == self.active_pipeline_id);
                if self.active_generation == Some(generation) || belongs_to_active_pipeline {
                    self.recover_poisoned_pipeline(
                        error,
                        device_lost,
                        report.map(|report| *report),
                    );
                }
            }
        }
    }

    fn recover_poisoned_pipeline(
        &mut self,
        error: String,
        device_lost: bool,
        report: Option<GpuFaultReport>,
    ) {
        let resume_continuous = self.continuous;
        self.metrics
            .record_gpu_drop(GpuDropReason::PipelinePoisoned);
        let (reported_pipeline_id, prepare_id) = self.persist_gpu_fault_reports(report);
        let pipeline_id = reported_pipeline_id.or(self.active_pipeline_id);
        self.gpu_pipeline_state("Poisoned", pipeline_id, prepare_id.as_deref(), None, None);
        let can_attempt_reprepare = should_auto_reprepare(&mut self.auto_reprepare_attempted);
        self.gpu_pipeline_state("Teardown", pipeline_id, prepare_id.as_deref(), None, None);
        self.suspend_invalid_pipeline(error.clone());
        if !can_attempt_reprepare {
            let reason = if device_lost {
                "repeated_device_removed_after_single_reprepare"
            } else {
                "repeated_fatal_gpu_fault_after_single_reprepare"
            };
            self.gpu_pipeline_state(
                "GpuPipelineInvalid",
                pipeline_id,
                prepare_id.as_deref(),
                Some(1),
                Some(reason),
            );
            self.edit_status(|status| {
                status.run.error = Some(format!(
                    "{error}; GPU pipeline invalid after its single automatic reprepare"
                ));
            });
            return;
        }

        self.gpu_pipeline_state(
            "Preparing",
            pipeline_id,
            prepare_id.as_deref(),
            Some(1),
            None,
        );
        tracing::warn!(%error, "repreparing complete GPU pipeline once after transport poison");
        self.retry_suspended_pipeline();
        if resume_continuous && self.model_ready {
            self.set_continuous_inner(true);
        }
        if !self.model_ready {
            self.gpu_pipeline_state(
                "GpuPipelineInvalid",
                pipeline_id,
                prepare_id.as_deref(),
                Some(1),
                Some("automatic_reprepare_failed"),
            );
        }
    }

    fn persist_gpu_fault_reports(
        &mut self,
        fallback: Option<GpuFaultReport>,
    ) -> (Option<u64>, Option<String>) {
        let reports = fallback.into_iter().collect::<Vec<_>>();
        let primary = reports
            .first()
            .map(|report| (Some(report.pipeline_id), Some(report.prepare_id.clone())))
            .unwrap_or((None, None));
        for report in reports {
            report.log();
            self.gpu_fault_history.push_back(report);
            while self.gpu_fault_history.len() > 4 {
                self.gpu_fault_history.pop_front();
            }
        }
        primary
    }

    fn handle_completed_inference(&mut self, result: InferenceFrameResult) -> Result<()> {
        if !is_active_generation(self.active_generation, result.metadata.generation) {
            tracing::debug!(
                generation = result.metadata.generation,
                "discarding old-generation inference result"
            );
            return Ok(());
        }
        if is_causally_stale(self.last_motion_complete_qpc, result.metadata.present_qpc) {
            self.metrics.record_causal_discard();
            tracing::debug!(
                present_qpc = result.metadata.present_qpc,
                last_motion_complete_qpc = ?self.last_motion_complete_qpc,
                "discarding causally stale inference result"
            );
            if !self.continuous {
                let generation = result.metadata.generation;
                self.active_generation = None;
                let _ = self.inference_worker.stop(generation);
                let _ = self.capture_worker.stop();
                self.edit_status(|status| {
                    status.run.phase = RunPhase::Idle;
                    status.performance = self.metrics.snapshot();
                });
            }
            return Ok(());
        }
        let one_shot = !self.continuous;
        if !one_shot && !self.continuous {
            return Ok(());
        }
        let output_started_qpc = self.qpc.now();
        let (last_target, last_command, pulse_controller) =
            self.write_detection_output(&result, one_shot)?;
        let output_complete_qpc = self.qpc.now();
        let mak_write_ms = self.qpc.elapsed_ms(output_started_qpc, output_complete_qpc);
        let inference_span_ms = self
            .qpc
            .elapsed_ms(result.inference_start_qpc, result.inference_end_qpc);
        let total_ms = self
            .qpc
            .elapsed_ms(result.metadata.present_qpc, output_complete_qpc);
        let timings = StageTimings {
            present_to_gpu_submit_ms: self
                .qpc
                .elapsed_ms(result.metadata.present_qpc, result.metadata.ready_qpc),
            present_to_inference_start_ms: self
                .qpc
                .elapsed_ms(result.metadata.present_qpc, result.inference_start_qpc),
            acquire_wait_ms: result.metadata.acquire_wait_ms,
            capture_ms: result.metadata.capture_ms,
            copy_submit_ms: result.metadata.copy_submit_ms,
            gpu_preprocess_ms: result.gpu_preprocess_ms,
            queue_age_ms: self
                .qpc
                .elapsed_ms(result.metadata.ready_qpc, result.inference_start_qpc),
            ort_run_wall_ms: result.ort_run_wall_ms,
            postprocess_ms: result.postprocess_ms,
            mak_write_ms,
            total_ms,
        };
        self.metrics
            .record_qpc(self.qpc, result.inference_start_qpc, timings);
        if last_command.is_some() {
            self.last_motion_complete_qpc = Some(output_complete_qpc);
        }
        if result.detection.is_some() {
            self.log_summary_detections += 1;
        }
        self.log_summary_outputs += 1;
        self.log_summary_frames += 1;
        if let Some(command) = last_command {
            self.log_summary_last_command = Some(command);
        }
        let performance = self.metrics.snapshot_if_due(one_shot);
        let physical = self.mak.physical_controller_status();
        let release_pending = self.mak.controller_release_pending();
        self.edit_status(|status| {
            status.capture.desktop_size = Some(result.metadata.desktop_size);
            status.run.last_target = last_target;
            status.run.last_command = last_command;
            status.run.last_total_ms = Some(total_ms);
            status.run.error = None;
            status.mak.physical_controller = physical;
            status.mak.controller_release_pending = release_pending;
            if let Some(performance) = performance {
                status.performance = performance;
            }
        });
        if crate::logging::frame_trace_enabled() {
            tracing::trace!(
                pipeline_id = result.metadata.pipeline_id,
                generation = result.metadata.generation,
                frame_seq = result.metadata.frame_seq,
                slot_id = result.metadata.slot_id,
                acquire_wait_ms = result.metadata.acquire_wait_ms,
                capture_ms = result.metadata.capture_ms,
                gpu_preprocess_ms = result.gpu_preprocess_ms,
                queue_age_ms = timings.queue_age_ms,
                present_qpc = result.metadata.present_qpc,
                accumulated_frames = result.metadata.accumulated_frames,
                ort_run_wall_ms = result.ort_run_wall_ms,
                inference_span_ms,
                mak_write_ms,
                total_ms,
                injection_api = ?self.mak.injection_api(),
                target = ?last_target,
                command = ?last_command,
                "frame pipeline completed"
            );
        }
        if pulse_controller {
            thread::sleep(CONTROLLER_ONE_SHOT_PULSE);
            self.mak.release_controller()?;
            self.sync_release_pending_status();
        }
        if one_shot {
            self.active_generation = None;
            let generation = result.metadata.generation;
            let _ = self.inference_worker.stop(generation);
            let _ = self.capture_worker.stop();
            self.edit_status(|status| status.run.phase = RunPhase::Idle);
        } else {
            self.log_continuous_summary_if_due();
        }
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    fn write_detection_output(
        &mut self,
        result: &InferenceFrameResult,
        one_shot: bool,
    ) -> Result<(
        Option<(f32, f32, f32, i32)>,
        Option<(i32, i32, OutputMode)>,
        bool,
    )> {
        let Some(detection) = result.detection else {
            self.mak.release_controller()?;
            return Ok((None, None, false));
        };
        if !is_active_generation(self.active_generation, result.metadata.generation) {
            return Ok((None, None, false));
        }
        let (width, height) = self
            .status
            .lock()
            .map(|status| status.model.input_size.unwrap_or((0, 0)))
            .unwrap_or((0, 0));
        let dx = detection.center_x - width as f32 * 0.5;
        let dy = detection.center_y - height as f32 * 0.5;
        let command_x = quantize(dx * self.sensitivity.gain());
        let relative_y_offset = detection.box_height * self.target_y_offset.value() as f32 / 100.0;
        let command_y = quantize(dy * self.sensitivity.gain() + relative_y_offset);
        if command_x == 0 && command_y == 0 {
            self.mak.release_controller()?;
            return Ok((
                Some((dx, dy, detection.confidence, detection.class_id)),
                None,
                false,
            ));
        }
        let command = match self.output_mode {
            OutputMode::Mouse => OutputCommand::MouseMove {
                x: command_x,
                y: command_y,
            },
            OutputMode::Controller => OutputCommand::ControllerState {
                x: command_x,
                y: command_y,
            },
        };
        self.write_output(command)?;
        Ok((
            Some((dx, dy, detection.confidence, detection.class_id)),
            Some((command_x, command_y, self.output_mode)),
            one_shot && self.output_mode == OutputMode::Controller,
        ))
    }

    fn write_output(&mut self, command: OutputCommand) -> Result<()> {
        match command {
            OutputCommand::MouseMove { x, y } => self.mak.move_mouse(x, y),
            OutputCommand::ControllerState { x, y } => self.mak.set_right_stick(x, y),
        }
    }

    fn reset_log_summary(&mut self) {
        self.log_summary_started = Instant::now();
        self.log_summary_frames = 0;
        self.log_summary_detections = 0;
        self.log_summary_outputs = 0;
        self.log_summary_last_command = None;
    }

    fn log_continuous_summary_if_due(&mut self) {
        let elapsed = self.log_summary_started.elapsed();
        if elapsed < CONTINUOUS_LOG_INTERVAL {
            return;
        }
        let performance = self.metrics.snapshot();
        tracing::debug!(
            elapsed_ms = elapsed.as_secs_f64() * 1000.0,
            frames = self.log_summary_frames,
            detections = self.log_summary_detections,
            output_submissions = self.log_summary_outputs,
            last_command = ?self.log_summary_last_command,
            fps = performance.fps,
            capture_p50_ms = performance.capture.p50_ms,
            inference_p50_ms = performance.inference.p50_ms,
            mak_write_p50_ms = performance.mak_write.p50_ms,
            pipeline_p50_ms = performance.total.p50_ms,
            superseded_before_claim = performance.gpu_drop_counts.get(GpuDropReason::SupersededBeforeClaim),
            no_free_slot = performance.gpu_drop_counts.get(GpuDropReason::NoFreeSlot),
            stale_generation = performance.gpu_drop_counts.get(GpuDropReason::StaleGeneration),
            reuse_not_complete = performance.gpu_drop_counts.get(GpuDropReason::ReuseNotComplete),
            pipeline_poisoned = performance.gpu_drop_counts.get(GpuDropReason::PipelinePoisoned),
            result_superseded = performance.gpu_drop_counts.get(GpuDropReason::ResultSuperseded),
            "continuous inference summary"
        );
        self.reset_log_summary();
    }

    fn emit_event(&self, event: EngineEvent) {
        let _ = self.event_tx.send(event);
        self.ui_ctx.request_repaint();
    }

    fn edit_status(&self, edit: impl FnOnce(&mut EngineStatus)) {
        {
            let mut status = self
                .status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            edit(&mut status);
        }
        self.ui_ctx.request_repaint();
    }
}

fn quantize(value: f32) -> i32 {
    value.round().clamp(i16::MIN as f32, i16::MAX as f32) as i32
}

fn should_auto_reprepare(attempted: &mut bool) -> bool {
    if *attempted {
        return false;
    }
    *attempted = true;
    true
}

fn is_active_generation(active: Option<RunGeneration>, result: RunGeneration) -> bool {
    active == Some(result)
}

fn is_causally_stale(last_motion_complete_qpc: Option<i64>, present_qpc: i64) -> bool {
    last_motion_complete_qpc.is_some_and(|last| present_qpc <= last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_full_reprepare_is_allowed_once_only() {
        let mut attempted = false;
        assert!(should_auto_reprepare(&mut attempted));
        assert!(attempted);
        assert!(!should_auto_reprepare(&mut attempted));
    }

    #[test]
    fn device_loss_allows_one_automatic_reprepare() {
        let mut attempted = false;
        assert!(should_auto_reprepare(&mut attempted));
        assert!(attempted);
    }

    #[test]
    fn command_quantization_uses_i16_bounds() {
        assert_eq!(quantize(1.6), 2);
        assert_eq!(quantize(-1.6), -2);
        assert_eq!(quantize(99999.0), i16::MAX as i32);
        assert_eq!(quantize(-99999.0), i16::MIN as i32);
    }

    #[test]
    fn target_y_offset_is_relative_to_target_height_and_added_after_sensitivity() {
        let sensitivity = Sensitivity::new(50);
        let target_height = 40.0;
        let offset = target_height * TargetYOffset::new(25).value() as f32 / 100.0;
        let command_y = quantize(20.0 * sensitivity.gain() + offset);
        assert_eq!(command_y, 20);
    }

    #[test]
    fn stale_generation_is_rejected() {
        assert!(is_active_generation(Some(7), 7));
        assert!(!is_active_generation(Some(7), 6));
        assert!(!is_active_generation(None, 7));
    }

    #[test]
    fn causal_gate_rejects_equal_and_older_present_times() {
        assert!(!is_causally_stale(None, 10));
        assert!(is_causally_stale(Some(10), 9));
        assert!(is_causally_stale(Some(10), 10));
        assert!(!is_causally_stale(Some(10), 11));
    }
}
