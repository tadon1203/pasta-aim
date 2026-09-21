use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use eframe::egui;

use crate::{
    capture::DxgiCapture,
    config::{Config, OutputMode, Sensitivity},
    detector::Detector,
    mak_output::{MakConnectionEvent, MakMetadata, MakOutput, MakRoutes, PhysicalControllerStatus},
    metrics::{Metrics, PerformanceSnapshot, StageTimings},
    models,
};

const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const PHYSICAL_CONTROLLER_POLL_INTERVAL: Duration = Duration::from_secs(5);
const CONTROLLER_ONE_SHOT_PULSE: Duration = Duration::from_millis(16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPhase {
    Empty,
    Loading,
    Ready,
}

#[derive(Debug, Clone)]
pub struct ModelStatus {
    pub phase: ModelPhase,
    pub active_name: Option<String>,
    pub loading_name: Option<String>,
    pub input_size: Option<(usize, usize)>,
    pub error: Option<String>,
}

impl Default for ModelStatus {
    fn default() -> Self {
        Self {
            phase: ModelPhase::Empty,
            active_name: None,
            loading_name: None,
            input_size: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePhase {
    Initializing,
    Ready,
    Failed,
}

#[derive(Debug, Clone)]
pub struct CaptureStatus {
    pub phase: CapturePhase,
    pub error: Option<String>,
    pub desktop_size: Option<(usize, usize)>,
}

impl Default for CaptureStatus {
    fn default() -> Self {
        Self {
            phase: CapturePhase::Initializing,
            error: None,
            desktop_size: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MakPhase {
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

#[derive(Debug, Clone)]
pub struct MakStatus {
    pub phase: MakPhase,
    pub port: Option<String>,
    pub firmware: Option<u32>,
    pub routes: MakRoutes,
    pub physical_controller: PhysicalControllerStatus,
    pub error: Option<String>,
}

impl Default for MakStatus {
    fn default() -> Self {
        Self {
            phase: MakPhase::Connecting,
            port: None,
            firmware: None,
            routes: MakRoutes::default(),
            physical_controller: PhysicalControllerStatus::Unknown,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhase {
    Idle,
    OneShot,
    Continuous,
}

#[derive(Debug, Clone)]
pub struct RunStatus {
    pub phase: RunPhase,
    pub error: Option<String>,
    pub last_target: Option<(f32, f32, f32, i32)>,
    pub last_command: Option<(i32, i32, OutputMode)>,
    pub last_total_ms: Option<f64>,
}

impl Default for RunStatus {
    fn default() -> Self {
        Self {
            phase: RunPhase::Idle,
            error: None,
            last_target: None,
            last_command: None,
            last_total_ms: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EngineStatus {
    pub model: ModelStatus,
    pub capture: CaptureStatus,
    pub mak: MakStatus,
    pub run: RunStatus,
    pub performance: PerformanceSnapshot,
}

impl EngineStatus {
    pub fn ready_for(&self, mode: OutputMode) -> bool {
        self.block_reason(mode).is_none()
    }

    pub fn block_reason(&self, mode: OutputMode) -> Option<&'static str> {
        if self.model.phase != ModelPhase::Ready {
            return Some("Model is not ready");
        }
        if self.capture.phase != CapturePhase::Ready {
            return Some("DXGI capture is not ready");
        }
        if self.mak.phase != MakPhase::Connected || self.mak.error.is_some() {
            return Some("MAK is not connected");
        }
        match mode {
            OutputMode::Mouse if !self.mak.routes.mouse => Some("Mouse route is unavailable"),
            OutputMode::Controller if !self.mak.routes.controller_available() => {
                Some("Controller route is unavailable")
            }
            _ => None,
        }
    }
}

enum Command {
    LoadModel { name: String, path: PathBuf },
    SetSensitivity(Sensitivity),
    SetOutput(OutputMode),
    SetContinuous(bool),
    RunOnce,
    RefreshMakRoutes,
    RescanMak,
    RetryCapture,
    ProbeController,
    Shutdown,
}

#[derive(Clone)]
pub struct EngineControl {
    tx: Sender<Command>,
}

impl EngineControl {
    pub fn set_continuous(&self, active: bool) {
        let _ = self.tx.send(Command::SetContinuous(active));
    }

    pub fn run_once(&self) {
        let _ = self.tx.send(Command::RunOnce);
    }
}

pub struct Engine {
    tx: Sender<Command>,
    status: Arc<Mutex<EngineStatus>>,
    join: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn new(config: &Config, ui_ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(EngineStatus::default()));
        let worker_status = Arc::clone(&status);
        let initial = config.clone();
        let join = thread::Builder::new()
            .name("nice-aimassist-engine".to_owned())
            .spawn(move || run_worker(rx, worker_status, initial, ui_ctx))
            .expect("failed to spawn engine worker");

        Self {
            tx,
            status,
            join: Some(join),
        }
    }

    pub fn control(&self) -> EngineControl {
        EngineControl {
            tx: self.tx.clone(),
        }
    }

    pub fn status(&self) -> EngineStatus {
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn load_model(&self, name: String, path: PathBuf) {
        let _ = self.tx.send(Command::LoadModel { name, path });
    }

    pub fn set_sensitivity(&self, value: Sensitivity) {
        let _ = self.tx.send(Command::SetSensitivity(value));
    }

    pub fn set_output(&self, mode: OutputMode) {
        let _ = self.tx.send(Command::SetOutput(mode));
    }

    pub fn run_once(&self) {
        let _ = self.tx.send(Command::RunOnce);
    }

    pub fn refresh_mak_routes(&self) {
        let _ = self.tx.send(Command::RefreshMakRoutes);
    }

    pub fn rescan_mak(&self) {
        let _ = self.tx.send(Command::RescanMak);
    }

    pub fn retry_capture(&self) {
        let _ = self.tx.send(Command::RetryCapture);
    }

    pub fn probe_controller(&self) {
        let _ = self.tx.send(Command::ProbeController);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct Worker {
    status: Arc<Mutex<EngineStatus>>,
    ui_ctx: egui::Context,
    detector: Option<Detector>,
    capture: Option<DxgiCapture>,
    mak: MakOutput,
    metrics: Metrics,
    sensitivity: Sensitivity,
    output_mode: OutputMode,
    continuous: bool,
    last_physical_probe: Instant,
}

fn run_worker(
    rx: Receiver<Command>,
    status: Arc<Mutex<EngineStatus>>,
    config: Config,
    ui_ctx: egui::Context,
) {
    let mut worker = Worker {
        status,
        ui_ctx,
        detector: None,
        capture: None,
        mak: MakOutput::new(),
        metrics: Metrics::new(),
        sensitivity: config.sensitivity,
        output_mode: config.output_mode,
        continuous: false,
        last_physical_probe: Instant::now() - PHYSICAL_CONTROLLER_POLL_INTERVAL,
    };

    worker.initialize_capture();
    worker.connect_mak_initial();
    if let Some(name) = config.model {
        match models::resolve_model(&name) {
            Ok(path) => worker.load_model(name, path),
            Err(error) => worker.edit_status(|status| {
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

            if let Err(error) = worker.step(false) {
                worker.stop_after_run_error(format!("Inference failed: {error:#}"));
                thread::sleep(Duration::from_millis(1));
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

impl Worker {
    fn handle_command(&mut self, command: Command) -> bool {
        match command {
            Command::LoadModel { name, path } => self.load_model(name, path),
            Command::SetSensitivity(value) => self.sensitivity = value,
            Command::SetOutput(mode) => {
                self.stop_continuous();
                if self.output_mode == OutputMode::Controller && mode != OutputMode::Controller {
                    self.mak.release_controller_best_effort();
                }
                self.output_mode = mode;
                if mode == OutputMode::Controller {
                    // Refresh the physical status promptly when the user chooses
                    // controller output, without polling it continuously on Mouse.
                    self.last_physical_probe = Instant::now() - PHYSICAL_CONTROLLER_POLL_INTERVAL;
                }
                self.clear_run_error();
            }
            Command::SetContinuous(active) => self.set_continuous(active),
            Command::RunOnce => self.run_once(),
            Command::RefreshMakRoutes => self.refresh_mak_routes(),
            Command::RescanMak => self.rescan_mak(),
            Command::RetryCapture => {
                self.stop_continuous();
                self.initialize_capture();
            }
            Command::ProbeController => self.probe_physical_controller(),
            Command::Shutdown => {
                self.stop_continuous();
                self.mak.release_controller_best_effort();
                return false;
            }
        }
        true
    }

    fn initialize_capture(&mut self) {
        self.capture = None;
        self.edit_status(|status| {
            status.capture.phase = CapturePhase::Initializing;
            status.capture.error = None;
            status.capture.desktop_size = None;
        });

        match DxgiCapture::new() {
            Ok(capture) => {
                self.capture = Some(capture);
                self.edit_status(|status| {
                    status.capture.phase = CapturePhase::Ready;
                    status.capture.error = None;
                });
            }
            Err(error) => {
                self.edit_status(|status| {
                    status.capture.phase = CapturePhase::Failed;
                    status.capture.error = Some(format!("{error:#}"));
                });
            }
        }
    }

    fn load_model(&mut self, name: String, path: PathBuf) {
        self.stop_continuous();
        self.mak.release_controller_best_effort();
        self.edit_status(|status| {
            status.model.phase = ModelPhase::Loading;
            status.model.loading_name = Some(name.clone());
            status.model.error = None;
            status.run.error = None;
        });

        match Detector::load(&path) {
            Ok(detector) => {
                let size = detector.input_size();
                self.detector = Some(detector);
                self.metrics.mark_cold_next();
                self.edit_status(|status| {
                    status.model.phase = ModelPhase::Ready;
                    status.model.active_name = Some(name);
                    status.model.loading_name = None;
                    status.model.input_size = Some(size);
                    status.model.error = None;
                });
            }
            Err(error) => {
                let has_active_model = self.detector.is_some();
                self.edit_status(|status| {
                    status.model.phase = if has_active_model {
                        ModelPhase::Ready
                    } else {
                        ModelPhase::Empty
                    };
                    status.model.loading_name = None;
                    status.model.error = Some(format!("{error:#}"));
                });
            }
        }
    }

    fn connect_mak_initial(&mut self) {
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
        self.stop_continuous();
        self.mak.release_controller_best_effort();
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
        self.stop_continuous();
        self.mak.release_controller_best_effort();
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
                    self.stop_continuous();
                    self.edit_status(|status| {
                        status.mak.phase = MakPhase::Reconnecting;
                        status.mak.routes = MakRoutes::default();
                        status.mak.physical_controller = PhysicalControllerStatus::Unknown;
                        status.mak.error = None;
                    });
                }
                MakConnectionEvent::Connected(Ok(metadata)) => self.apply_mak_metadata(metadata),
                MakConnectionEvent::Connected(Err(error)) => self.set_mak_failure(error),
            }
        }
    }

    fn apply_mak_metadata(&mut self, metadata: MakMetadata) {
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
            status.mak.error = None;
        });
    }

    fn set_mak_failure(&mut self, error: String) {
        self.stop_continuous();
        self.edit_status(|status| {
            status.mak.phase = MakPhase::Failed;
            status.mak.routes = MakRoutes::default();
            status.mak.physical_controller = PhysicalControllerStatus::Unknown;
            status.mak.error = Some(error);
        });
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
            self.edit_status(|status| status.run.error = Some(reason));
            return;
        }

        self.continuous = true;
        self.edit_status(|status| {
            status.run.phase = RunPhase::Continuous;
            status.run.error = None;
        });
    }

    fn run_once(&mut self) {
        if self.continuous {
            self.edit_status(|status| {
                status.run.error = Some("Stop continuous inference before running once".to_owned());
            });
            return;
        }
        if let Some(reason) = self.run_block_reason() {
            self.edit_status(|status| status.run.error = Some(reason));
            return;
        }

        self.edit_status(|status| {
            status.run.phase = RunPhase::OneShot;
            status.run.error = None;
        });
        if let Err(error) = self.step(true) {
            self.mak.release_controller_best_effort();
            self.edit_status(|status| {
                status.run.phase = RunPhase::Idle;
                status.run.error = Some(format!("Single inference failed: {error:#}"));
            });
        } else {
            self.edit_status(|status| status.run.phase = RunPhase::Idle);
        }
    }

    fn stop_continuous(&mut self) {
        if self.continuous {
            self.continuous = false;
            self.mak.release_controller_best_effort();
        }
        self.edit_status(|status| {
            if status.run.phase == RunPhase::Continuous {
                status.run.phase = RunPhase::Idle;
            }
        });
    }

    fn stop_after_run_error(&mut self, error: String) {
        self.continuous = false;
        self.mak.release_controller_best_effort();
        self.edit_status(|status| {
            status.run.phase = RunPhase::Idle;
            status.run.error = Some(error);
        });
    }

    fn clear_run_error(&self) {
        self.edit_status(|status| status.run.error = None);
    }

    fn run_block_reason(&self) -> Option<String> {
        if self.detector.is_none() {
            return Some("Model is not ready".to_owned());
        }
        if self.capture.is_none() {
            return Some("DXGI capture is not ready".to_owned());
        }
        if !self.mak.is_connected() {
            return Some("MAK is not connected".to_owned());
        }
        match self.output_mode {
            OutputMode::Mouse if !self.mak.mouse_available() => {
                Some("Mouse route is unavailable".to_owned())
            }
            OutputMode::Controller if !self.mak.controller_available() => {
                Some("Controller route is unavailable".to_owned())
            }
            _ => None,
        }
    }

    fn step(&mut self, one_shot: bool) -> Result<()> {
        if let Some(reason) = self.run_block_reason() {
            return Err(anyhow!(reason));
        }

        let started = Instant::now();
        let detector = self
            .detector
            .as_mut()
            .ok_or_else(|| anyhow!("no model loaded"))?;
        let (input_width, input_height) = detector.input_size();
        let capture = self
            .capture
            .as_mut()
            .ok_or_else(|| anyhow!("DXGI capture is not initialized"))?;

        let frame = capture.capture_center(input_width, input_height)?;
        let capture_ms = frame.capture_ms;
        let preprocess_ms = frame.preprocess_ms;
        let desktop_size = frame.desktop_size;
        let inference = detector.infer_nearest(frame.nchw_rgb)?;
        let gain = self.sensitivity.gain();

        let (output_ms, last_target, last_command, pulse_controller) =
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
                    self.mak.send(self.output_mode, command_x, command_y)?;
                    one_shot && self.output_mode == OutputMode::Controller
                };
                let output_ms = output_started.elapsed().as_secs_f64() * 1000.0;

                (
                    output_ms,
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

        let total_ms = started.elapsed().as_secs_f64() * 1000.0;
        let timings = StageTimings {
            capture_ms,
            preprocess_ms,
            inference_ms: inference.inference_ms,
            postprocess_ms: inference.postprocess_ms,
            output_ms,
            total_ms,
        };
        self.metrics.record(started, timings);
        let performance = self.metrics.snapshot_if_due(one_shot);
        let physical = self.mak.physical_controller_status();

        if one_shot || performance.is_some() {
            self.edit_status(|status| {
                status.capture.desktop_size = Some(desktop_size);
                status.run.last_target = last_target;
                status.run.last_command = last_command;
                status.run.last_total_ms = Some(total_ms);
                status.run.error = None;
                status.mak.physical_controller = physical;
                if let Some(performance) = performance {
                    status.performance = performance;
                }
            });
        }

        if pulse_controller {
            thread::sleep(CONTROLLER_ONE_SHOT_PULSE);
            self.mak.release_controller()?;
        }

        Ok(())
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
}
