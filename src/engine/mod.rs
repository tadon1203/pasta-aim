mod capture_worker;
mod inference_worker;
mod status;
mod worker;

pub(super) type RunGeneration = u64;

pub use status::{
    CapturePhase, EngineStatus, MakPhase, ModelPhase, RunPhase, cpu_fallback_nodes,
    ep_assignment_summary, provider_display_name,
};

use worker::run_worker;

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
};

use eframe::egui;

use crate::{
    config::Config,
    settings::{
        ConfidenceThreshold, DirectMlDevice, FpsLimit, InjectionApi, OutputMode, Sensitivity,
        TargetYOffset,
    },
};
use crossbeam_channel as crossbeam;

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Model {
        name: String,
        result: std::result::Result<(), String>,
    },
    Capture {
        adapter: u32,
        output: u32,
        result: std::result::Result<(), String>,
    },
    DirectMl {
        device: DirectMlDevice,
        result: std::result::Result<(), String>,
    },
}

#[derive(Debug)]
pub(super) enum Command {
    LoadModel {
        name: String,
        path: PathBuf,
        notify: bool,
    },
    SetSensitivity(Sensitivity),
    SetConfidenceThreshold(ConfidenceThreshold),
    SetTargetYOffset(TargetYOffset),
    SetFpsLimit(FpsLimit),
    SetOutput(OutputMode),
    SetInjectionApi(InjectionApi),
    SetCaptureSelection {
        adapter: u32,
        output: u32,
    },
    SetDirectMlDevice(DirectMlDevice),
    SetContinuous(bool),
    RunOnce,
    RefreshMakRoutes,
    RescanMak,
    RetryCapture,
    ProbeController,
    RescanDisplays,
    Shutdown,
}

#[derive(Clone)]
pub struct EngineControl {
    tx: crossbeam::Sender<Command>,
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
    tx: crossbeam::Sender<Command>,
    events: Receiver<EngineEvent>,
    status: Arc<Mutex<EngineStatus>>,
    join: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn new(config: &Config, ui_ctx: egui::Context) -> Self {
        tracing::info!("spawning engine worker");
        let (tx, rx) = crossbeam::unbounded();
        let (event_tx, events) = mpsc::channel();
        let status = Arc::new(Mutex::new(EngineStatus::default()));
        let worker_status = Arc::clone(&status);
        let initial = config.clone();
        let join = thread::Builder::new()
            .name("nice-aimassist-engine".to_owned())
            .spawn(move || run_worker(rx, event_tx, worker_status, initial, ui_ctx))
            .expect("failed to spawn engine worker");

        Self {
            tx,
            events,
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

    pub fn poll_events(&self) -> impl Iterator<Item = EngineEvent> + '_ {
        std::iter::from_fn(|| self.events.try_recv().ok())
    }

    pub fn load_model(&self, name: String, path: PathBuf) {
        let _ = self.tx.send(Command::LoadModel {
            name,
            path,
            notify: true,
        });
    }

    pub(crate) fn load_model_transient(&self, name: String, path: PathBuf) {
        let _ = self.tx.send(Command::LoadModel {
            name,
            path,
            notify: false,
        });
    }

    pub fn set_sensitivity(&self, value: Sensitivity) {
        let _ = self.tx.send(Command::SetSensitivity(value));
    }

    pub fn set_confidence_threshold(&self, value: ConfidenceThreshold) {
        let _ = self.tx.send(Command::SetConfidenceThreshold(value));
    }

    pub fn set_target_y_offset(&self, value: TargetYOffset) {
        let _ = self.tx.send(Command::SetTargetYOffset(value));
    }

    pub fn set_fps_limit(&self, value: FpsLimit) {
        let _ = self.tx.send(Command::SetFpsLimit(value));
    }

    pub fn set_output(&self, mode: OutputMode) {
        let _ = self.tx.send(Command::SetOutput(mode));
    }

    pub fn set_injection_api(&self, api: InjectionApi) {
        let _ = self.tx.send(Command::SetInjectionApi(api));
    }

    pub fn set_capture_selection(&self, adapter: u32, output: u32) {
        let _ = self
            .tx
            .send(Command::SetCaptureSelection { adapter, output });
    }

    pub fn set_directml_device(&self, device: DirectMlDevice) {
        let _ = self.tx.send(Command::SetDirectMlDevice(device));
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

    pub fn rescan_displays(&self) {
        let _ = self.tx.send(Command::RescanDisplays);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        tracing::info!("shutting down engine worker");
        let _ = self.tx.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
