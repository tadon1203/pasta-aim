use crate::sync::MutexExt;
mod capture_worker;
mod inference_worker;
mod output;
mod status;
mod telemetry;
mod worker;

pub(super) type RunGeneration = u64;

/// Whether a run keeps capturing until stopped or handles a single frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunMode {
    Continuous,
    OneShot,
}

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
        ConfidenceThreshold, FpsLimit, InjectionApi, MaxDelta, OutputMode, OverlayConfig,
        Sensitivity, TargetYOffset,
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
        adapter_luid: crate::inference::AdapterLuid,
        result: std::result::Result<(), String>,
    },
}

/// Requests accepted by the engine thread.
#[derive(Debug)]
pub enum Command {
    /// `notify` reports the outcome to the UI (which then persists the choice).
    LoadModel {
        name: String,
        path: PathBuf,
        notify: bool,
    },
    SetSensitivity(Sensitivity),
    SetConfidenceThreshold(ConfidenceThreshold),
    SetTargetYOffset(TargetYOffset),
    SetMaxDelta(MaxDelta),
    SetFpsLimit(FpsLimit),
    /// Reloads the active model, since the DirectML wait mode is a session option.
    SetLowLatency(bool),
    SetOutput(OutputMode),
    SetOverlay(OverlayConfig),
    SetInjectionApi(InjectionApi),
    SetCaptureSelection {
        adapter: u32,
        output: u32,
    },
    SetContinuous(bool),
    RunOnce,
    RefreshMakRoutes,
    RescanMak,
    RetryCapture,
    ProbeController,
    RescanDisplays,
    Shutdown,
}

/// Cheap, cloneable handle for sending commands to the engine thread.
#[derive(Clone)]
pub struct EngineControl {
    tx: crossbeam::Sender<Command>,
}

impl EngineControl {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }
}

pub struct Engine {
    control: EngineControl,
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
            control: EngineControl { tx },
            events,
            status,
            join: Some(join),
        }
    }

    pub fn control(&self) -> EngineControl {
        self.control.clone()
    }

    pub fn send(&self, command: Command) {
        self.control.send(command);
    }

    pub fn status(&self) -> EngineStatus {
        self.status.lock_recover().clone()
    }

    pub fn poll_events(&self) -> impl Iterator<Item = EngineEvent> + '_ {
        std::iter::from_fn(|| self.events.try_recv().ok())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        tracing::info!("shutting down engine worker");
        self.control.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
