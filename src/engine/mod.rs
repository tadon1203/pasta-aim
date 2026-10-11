use crate::sync::MutexExt;
mod output;
mod status;
pub(crate) mod target_track;
mod worker;

pub(crate) type RunGeneration = u64;

/// Whether a run keeps capturing until stopped or handles a single frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunMode {
    Continuous,
    OneShot,
}

pub use status::{
    CapturePhase, EngineState, MakcuPhase, ModelPhase, RunPhase, cpu_fallback_nodes,
    ep_assignment_summary, provider_display_name,
};

use worker::{WorkerLinks, run_worker};

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
        CaptureSettings, ConfidenceThreshold, FpsLimit, Interpolation, MaxDelta, MoveMethod,
        Sensitivity, TargetYOffset,
    },
    telemetry::{LiveTelemetry, Telemetry},
    timing::QpcClock,
};
use crossbeam_channel as crossbeam;

use crate::wait::{DeadlineWait, Waker};

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
    SetMoveMethod(MoveMethod),
    SetInterpolation(Interpolation),
    SetFpsLimit(FpsLimit),
    SetCaptureSettings(CaptureSettings),
    /// Reloads the active model, since the DirectML wait mode is a session option.
    SetLowLatency(bool),
    /// Applies to the next run.
    SetFrameTrace(bool),
    SetCaptureSelection {
        adapter: u32,
        output: u32,
    },
    SetContinuous(bool),
    RunOnce,
    RefreshMakcuRoutes,
    RescanMakcu,
    RetryCapture,
    RescanDisplays,
    Shutdown,
}

/// Cheap, cloneable handle for sending commands to the engine thread.
#[derive(Clone)]
pub struct EngineControl {
    tx: crossbeam::Sender<Command>,
    waker: Waker,
}

impl EngineControl {
    /// Sends the command, then wakes the engine so that it reads it at once.
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
        self.waker.wake();
    }
}

pub struct Engine {
    control: EngineControl,
    events: Receiver<EngineEvent>,
    state: Arc<Mutex<Arc<EngineState>>>,
    telemetry: Telemetry,
    join: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn new(config: &Config, ui_ctx: egui::Context) -> Self {
        tracing::info!("spawning engine worker");
        let (tx, rx) = crossbeam::unbounded();
        let (wait, waker) = DeadlineWait::new().expect("failed to create the engine wait");
        let (event_tx, events) = mpsc::channel();
        let qpc = QpcClock::new().expect("failed to read the QPC frequency");
        let (telemetry, sink) = Telemetry::spawn(qpc, ui_ctx.clone());
        let state = Arc::new(Mutex::new(Arc::new(EngineState::default())));
        let links = WorkerLinks {
            commands: rx,
            events: event_tx,
            state: Arc::clone(&state),
            ui_ctx,
            wait,
            qpc,
            telemetry: sink,
        };
        let initial = config.clone();
        let join = thread::Builder::new()
            .name("nice-aimassist-engine".to_owned())
            .spawn(move || run_worker(links, initial))
            .expect("failed to spawn engine worker");

        Self {
            control: EngineControl { tx, waker },
            events,
            state,
            telemetry,
            join: Some(join),
        }
    }

    pub fn control(&self) -> EngineControl {
        self.control.clone()
    }

    pub fn send(&self, command: Command) {
        self.control.send(command);
    }

    /// The engine state. The UI takes a clone of the `Arc`, which the engine replaces when the state changes.
    pub fn state(&self) -> Arc<EngineState> {
        Arc::clone(&self.state.lock_recover())
    }

    /// The values that the Telemetry thread publishes, at most ten times each second.
    pub fn live(&self) -> LiveTelemetry {
        self.telemetry.live()
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
        self.telemetry.stop();
    }
}
