use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use eframe::egui;

use super::status::{RunReadiness, run_block_reason};
use super::{
    CapturePhase, Command, EngineEvent, EngineStatus, MakPhase, ModelPhase, RunPhase,
    ep_assignment_summary,
};
use crate::{
    config::Config,
    detector::Detector,
    graphics::{CapturePoll, DxgiCapture, GraphicsThread, MappedCaptureFrame},
    mak::{MakClient, MakConnectionEvent, MakMetadata, MakRoutes, PhysicalControllerStatus},
    metrics::{Metrics, StageTimings},
    models,
    settings::{DirectMlDevice, FpsLimit, OutputMode, Sensitivity},
};

const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(100);
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
    detector: Option<Detector>,
    active_model: Option<(String, PathBuf)>,
    capture: Option<DxgiCapture>,
    capture_adapter: u32,
    capture_output: u32,
    directml_device: DirectMlDevice,
    mak: MakClient,
    metrics: Metrics,
    sensitivity: Sensitivity,
    fps_limit: FpsLimit,
    output_mode: OutputMode,
    continuous: bool,
    last_physical_probe: Instant,
    next_frame_due: Option<Instant>,
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

    let mut worker = Worker {
        event_tx,
        status,
        ui_ctx,
        detector: None,
        active_model: None,
        capture: None,
        capture_adapter: config.capture_adapter,
        capture_output: config.capture_output,
        directml_device: config.directml_device,
        mak: MakClient::new(),
        metrics: Metrics::new(),
        sensitivity: config.sensitivity,
        fps_limit: config.fps_limit,
        output_mode: config.output_mode,
        continuous: false,
        last_physical_probe: Instant::now() - PHYSICAL_CONTROLLER_POLL_INTERVAL,
        next_frame_due: None,
        log_summary_started: Instant::now(),
        log_summary_frames: 0,
        log_summary_detections: 0,
        log_summary_outputs: 0,
        log_summary_last_command: None,
        graphics,
    };

    worker.refresh_capture_catalog();
    worker.initialize_capture();
    worker.edit_status(|status| status.model.directml_device = config.directml_device);
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

        if worker.continuous {
            while let Ok(command) = rx.try_recv() {
                if !worker.handle_command(command) {
                    return;
                }
            }
            if !worker.continuous {
                continue;
            }

            if let Err(error) = worker.process_frame(false) {
                worker.stop_after_run_error(format!("Inference failed: {error:#}"));
                thread::sleep(Duration::from_millis(1));
                continue;
            }

            if !wait_for_frame_deadline(&rx, &mut worker) {
                return;
            }
        } else {
            match rx.recv_timeout(IDLE_POLL_INTERVAL) {
                Ok(command) => {
                    if !worker.handle_command(command) {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

fn wait_for_frame_deadline(rx: &Receiver<Command>, worker: &mut Worker) -> bool {
    while worker.continuous {
        let Some(deadline) = worker.next_frame_due else {
            break;
        };
        let wake_at = deadline;
        let now = Instant::now();
        if now >= wake_at {
            break;
        }

        match rx.recv_timeout(wake_at.saturating_duration_since(now)) {
            Ok(command) => {
                if !worker.handle_command(command) {
                    return false;
                }
            }
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
    true
}

impl Worker {
    fn handle_command(&mut self, command: Command) -> bool {
        tracing::debug!(?command, "engine command received");
        match command {
            Command::LoadModel { name, path, notify } => self.load_model(notify, name, path),
            Command::SetSensitivity(value) => self.sensitivity = value,
            Command::SetFpsLimit(value) => {
                self.fps_limit = value;
                self.next_frame_due = None;
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
            Command::SetCaptureSelection { adapter, output } => {
                self.set_capture_selection(adapter, output)
            }
            Command::SetDirectMlDevice(device) => self.set_directml_device(device),
            Command::SetContinuous(active) => self.set_continuous(active),
            Command::RunOnce => self.run_once(),
            Command::RefreshMakRoutes => self.refresh_mak_routes(),
            Command::RescanMak => self.rescan_mak(),
            Command::RetryCapture => {
                self.stop_continuous();
                self.initialize_capture();
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

        let adapters = graphics.enumerate_adapters();
        let displays = graphics.enumerate_displays();
        match (adapters, displays) {
            (Ok(adapters), Ok(displays)) => self.edit_status(|status| {
                status.graphics.adapters = adapters;
                status.graphics.displays = displays;
                status.graphics.catalog_error = None;
            }),
            (adapter_result, display_result) => {
                let error = adapter_result
                    .as_ref()
                    .err()
                    .or_else(|| display_result.as_ref().err())
                    .map(|error| format!("{error:#}"))
                    .unwrap_or_else(|| "graphics enumeration failed".to_owned());
                self.edit_status(|status| {
                    tracing::error!(%error, "graphics enumeration failed");
                    status.graphics.adapters = adapter_result.unwrap_or_default();
                    status.graphics.displays = display_result.unwrap_or_default();
                    status.graphics.catalog_error = Some(error);
                });
            }
        }
    }

    fn initialize_capture(&mut self) {
        tracing::info!(
            adapter = self.capture_adapter,
            output = self.capture_output,
            "initializing selected capture"
        );
        let Some(graphics) = self.graphics.as_ref() else {
            self.capture = None;
            self.edit_status(|status| {
                status.capture.phase = CapturePhase::Failed;
                status.capture.selected = None;
                if status.capture.error.is_none() {
                    status.capture.error =
                        Some("Graphics thread COM initialization is unavailable".to_owned());
                }
            });
            return;
        };

        self.edit_status(|status| {
            status.capture.phase = CapturePhase::Initializing;
            status.capture.error = None;
            status.capture.desktop_size = None;
        });

        match graphics.create_capture(self.capture_adapter, self.capture_output) {
            Ok(capture) => {
                let selected = capture.info().clone();
                self.capture = Some(capture);
                self.next_frame_due = None;
                self.edit_status(|status| {
                    status.capture.phase = CapturePhase::Ready;
                    status.capture.selected = Some(selected);
                    status.capture.error = None;
                });
                tracing::info!("selected capture initialized");
            }
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "selected capture initialization failed");
                self.capture = None;
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
        self.stop_continuous();
        let Some(graphics) = self.graphics.as_ref() else {
            let error = "Graphics thread COM initialization is unavailable".to_owned();
            self.initialize_capture();
            self.emit_event(EngineEvent::Capture {
                adapter,
                output,
                result: Err(error),
            });
            return;
        };
        match graphics.create_capture(adapter, output) {
            Ok(capture) => {
                let selected = capture.info().clone();
                self.capture = Some(capture);
                self.next_frame_due = None;
                self.capture_adapter = adapter;
                self.capture_output = output;
                self.metrics.mark_cold_next();
                self.edit_status(|status| {
                    status.capture.phase = CapturePhase::Ready;
                    status.capture.selected = Some(selected);
                    status.capture.desktop_size = None;
                    status.capture.error = None;
                    status.run.error = None;
                });
                self.emit_event(EngineEvent::Capture {
                    adapter,
                    output,
                    result: Ok(()),
                });
                tracing::info!(adapter, output, "capture selection changed");
            }
            Err(error) => {
                let message = format!("Display change failed: {error:#}");
                let has_active = self.capture.is_some();
                self.edit_status(|status| {
                    status.capture.phase = if has_active {
                        CapturePhase::Ready
                    } else {
                        CapturePhase::Failed
                    };
                    status.capture.error = Some(message.clone());
                });
                self.emit_event(EngineEvent::Capture {
                    adapter,
                    output,
                    result: Err(message.clone()),
                });
                tracing::error!(adapter, output, %message, "capture selection change failed");
            }
        }
    }

    fn load_model(&mut self, notify: bool, name: String, path: PathBuf) {
        tracing::info!(model = %name, path = %path.display(), ?self.directml_device, "loading model in engine");
        self.stop_continuous();
        self.mak.release_controller_best_effort();
        self.sync_release_pending_status();
        self.edit_status(|status| {
            status.model.phase = ModelPhase::Loading;
            status.model.loading_name = Some(name.clone());
            status.model.error = None;
            status.run.error = None;
        });

        match Detector::load(&path, self.directml_device) {
            Ok(detector) => {
                let size = detector.input_size();
                let device = detector.directml_device();
                let assignment = detector.ep_assignment().clone();
                let assignment_summary = ep_assignment_summary(&assignment);
                self.detector = Some(detector);
                self.active_model = Some((name.clone(), path));
                self.metrics.mark_cold_next();
                self.edit_status(|status| {
                    status.model.phase = ModelPhase::Ready;
                    status.model.active_name = Some(name.clone());
                    status.model.loading_name = None;
                    status.model.input_size = Some(size);
                    status.model.directml_device = device;
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
                let has_active_model = self.detector.is_some();
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

    fn set_directml_device(&mut self, requested: DirectMlDevice) {
        tracing::info!(?requested, "changing DirectML device");
        if requested == self.directml_device {
            self.emit_event(EngineEvent::DirectMl {
                device: requested,
                result: Ok(()),
            });
            return;
        }
        self.stop_continuous();

        let Some((name, path)) = self.active_model.clone() else {
            self.directml_device = requested;
            self.edit_status(|status| {
                status.model.directml_device = requested;
                status.model.error = None;
            });
            self.emit_event(EngineEvent::DirectMl {
                device: requested,
                result: Ok(()),
            });
            return;
        };

        self.edit_status(|status| {
            status.model.phase = ModelPhase::Loading;
            status.model.loading_name = Some(name.clone());
            status.model.error = None;
        });
        match Detector::load(&path, requested) {
            Ok(detector) => {
                let size = detector.input_size();
                let assignment = detector.ep_assignment().clone();
                let assignment_summary = ep_assignment_summary(&assignment);
                self.detector = Some(detector);
                self.directml_device = requested;
                self.metrics.mark_cold_next();
                self.edit_status(|status| {
                    status.model.phase = ModelPhase::Ready;
                    status.model.loading_name = None;
                    status.model.input_size = Some(size);
                    status.model.directml_device = requested;
                    status.model.error = None;
                    status.model.ep_assignment = Some(assignment);
                    status.model.ep_assignment_summary = assignment_summary;
                });
                self.emit_event(EngineEvent::DirectMl {
                    device: requested,
                    result: Ok(()),
                });
            }
            Err(error) => {
                let message = format!("DirectML device change failed: {error:#}");
                tracing::error!(?requested, %message, "DirectML device change failed");
                self.edit_status(|status| {
                    status.model.phase = ModelPhase::Ready;
                    status.model.loading_name = None;
                    status.model.error = Some(message.clone());
                });
                self.emit_event(EngineEvent::DirectMl {
                    device: requested,
                    result: Err(message),
                });
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

        self.continuous = true;
        self.next_frame_due = None;
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

        self.edit_status(|status| {
            status.run.phase = RunPhase::OneShot;
            status.run.error = None;
        });
        if let Err(error) = self.process_frame(true) {
            tracing::error!(error = %format!("{error:#}"), "single inference failed");
            self.mak.release_controller_best_effort();
            self.sync_release_pending_status();
            self.edit_status(|status| {
                status.run.phase = RunPhase::Idle;
                status.run.error = Some(format!("Single inference failed: {error:#}"));
            });
        } else {
            tracing::info!("single inference completed");
            self.edit_status(|status| status.run.phase = RunPhase::Idle);
        }
    }

    fn stop_continuous(&mut self) {
        if self.continuous {
            tracing::info!("continuous inference stopped");
            self.continuous = false;
            self.mak.release_controller_best_effort();
        }
        self.next_frame_due = None;
        let pending = self.mak.controller_release_pending();
        self.edit_status(|status| {
            if status.run.phase == RunPhase::Continuous {
                status.run.phase = RunPhase::Idle;
            }
            status.mak.controller_release_pending = pending;
        });
    }

    fn stop_after_run_error(&mut self, error: String) {
        tracing::error!(%error, "continuous inference stopped after error");
        self.continuous = false;
        self.next_frame_due = None;
        self.mak.release_controller_best_effort();
        let pending = self.mak.controller_release_pending();
        self.edit_status(|status| {
            status.run.phase = RunPhase::Idle;
            status.run.error = Some(error);
            status.mak.controller_release_pending = pending;
        });
    }

    fn clear_run_error(&self) {
        self.edit_status(|status| status.run.error = None);
    }

    fn run_block_reason(&self) -> Option<String> {
        run_block_reason(
            RunReadiness {
                model_ready: self.detector.is_some(),
                capture_ready: self.capture.is_some(),
                mak_connected: self.mak.is_connected(),
                mouse_route: self.mak.mouse_available(),
                controller_route: self.mak.controller_available(),
            },
            self.output_mode,
        )
        .map(str::to_owned)
    }

    fn process_frame(&mut self, one_shot: bool) -> Result<()> {
        if let Some(reason) = self.run_block_reason() {
            return Err(anyhow!(reason));
        }

        let detector = self
            .detector
            .as_mut()
            .ok_or_else(|| anyhow!("no model loaded"))?;
        let (input_width, input_height) = detector.input_size();
        let capture = self
            .capture
            .as_mut()
            .ok_or_else(|| anyhow!("DXGI capture is not initialized"))?;

        let scheduled_due = if one_shot { None } else { self.next_frame_due };
        let frame: MappedCaptureFrame<'_> =
            match capture.try_capture_center(input_width, input_height)? {
                CapturePoll::Fresh(frame) => frame,
                CapturePoll::NoDesktopUpdate => {
                    if !one_shot {
                        self.next_frame_due = Some(next_frame_deadline(
                            scheduled_due,
                            Instant::now(),
                            self.fps_limit.frame_interval(),
                        ));
                    }
                    return Ok(());
                }
            };
        let acquired_at = frame.acquired_at();
        let frame_wait_ms = frame.frame_wait_ms();
        let capture_ms = frame.capture_ms();
        let desktop_size = frame.desktop_size();
        let present_qpc = frame.present_qpc();
        let accumulated_frames = frame.accumulated_frames();
        if !one_shot {
            self.next_frame_due = Some(next_frame_deadline(
                scheduled_due,
                acquired_at,
                self.fps_limit.frame_interval(),
            ));
        }
        let inference = detector.infer_bgra(frame.bytes(), frame.row_pitch())?;
        drop(frame);
        let gain = self.sensitivity.gain();

        let (mak_write_ms, last_target, last_command, pulse_controller) =
            if let Some(detection) = inference.detection {
                let dx = detection.center_x - input_width as f32 * 0.5;
                let dy = detection.center_y - input_height as f32 * 0.5;
                let command_x = quantize(dx * gain);
                let command_y = quantize(dy * gain);

                let output_started = Instant::now();
                let pulse_controller = if command_x == 0 && command_y == 0 {
                    self.mak.release_controller()?;
                    false
                } else {
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
                    one_shot && self.output_mode == OutputMode::Controller
                };
                let mak_write_ms = output_started.elapsed().as_secs_f64() * 1000.0;

                (
                    mak_write_ms,
                    Some((dx, dy, detection.confidence, detection.class_id)),
                    Some((command_x, command_y, self.output_mode)),
                    pulse_controller,
                )
            } else {
                let output_started = Instant::now();
                self.mak.release_controller()?;
                (
                    output_started.elapsed().as_secs_f64() * 1000.0,
                    None,
                    None,
                    false,
                )
            };

        let total_ms = capture_ms
            + inference.preprocess_ms
            + inference.inference_ms
            + inference.postprocess_ms
            + mak_write_ms;
        let timings = StageTimings {
            frame_wait_ms,
            capture_ms,
            preprocess_ms: inference.preprocess_ms,
            inference_ms: inference.inference_ms,
            postprocess_ms: inference.postprocess_ms,
            mak_write_ms,
            total_ms,
        };
        self.metrics.record(acquired_at, timings);
        if inference.detection.is_some() {
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

        if one_shot || performance.is_some() {
            self.edit_status(|status| {
                status.capture.desktop_size = Some(desktop_size);
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
        }

        if crate::logging::frame_trace_enabled() {
            tracing::trace!(
                one_shot,
                frame_wait_ms,
                capture_ms,
                present_qpc,
                accumulated_frames,
                inference_ms = inference.inference_ms,
                mak_write_ms,
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
            tracing::debug!(target = ?last_target, command = ?last_command, total_ms, "single inference result");
        } else {
            self.log_continuous_summary_if_due();
        }

        Ok(())
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

fn next_frame_deadline(
    scheduled_due: Option<Instant>,
    acquired_at: Instant,
    interval: Duration,
) -> Instant {
    let mut next = scheduled_due
        .map(|deadline| deadline + interval)
        .unwrap_or(acquired_at + interval);
    while next <= acquired_at {
        next += interval;
    }
    next
}

fn quantize(value: f32) -> i32 {
    value.round().clamp(i16::MIN as f32, i16::MAX as f32) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_quantization_uses_i16_bounds() {
        assert_eq!(quantize(1.6), 2);
        assert_eq!(quantize(-1.6), -2);
        assert_eq!(quantize(99999.0), i16::MAX as i32);
        assert_eq!(quantize(-99999.0), i16::MIN as i32);
    }

    #[test]
    fn frame_deadline_keeps_a_stable_cadence_after_a_late_frame() {
        let base = Instant::now();
        let interval = Duration::from_millis(10);

        assert_eq!(next_frame_deadline(None, base, interval), base + interval);
        assert_eq!(
            next_frame_deadline(
                Some(base + interval),
                base + Duration::from_millis(25),
                interval
            ),
            base + Duration::from_millis(30),
        );
    }
}
