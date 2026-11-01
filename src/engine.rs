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

pub use status::{CapturePhase, EngineState, MakcuPhase, ModelPhase, RunPhase};

use worker::{WorkerLinks, run_worker};

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
};

use crate::{
    Repaint,
    clock::Clock,
    settings::{
        Confidence, FpsLimit, Interpolation, MaxDelta, NewFrameWait, Sensitivity, Settings,
        TargetYOffset,
    },
    telemetry::{LiveTelemetry, Telemetry},
};

use crate::clock::{self, Wake};

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Model {
        name: String,
        result: std::result::Result<(), String>,
    },
    Capture {
        adapter: u32,
        output: u32,
        adapter_luid: crate::pipeline::AdapterLuid,
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
    SetConfidence(Confidence),
    SetTargetYOffset(TargetYOffset),
    SetMaxDelta(MaxDelta),
    SetInterpolation(Interpolation),
    SetFpsLimit(FpsLimit),
    SetNewFrameWait(NewFrameWait),
    /// Reloads the active model, since the `DirectML` wait mode is a session option.
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
    /// Chooses the MAKCU port (`None` is auto-detect) and connects again.
    SetMakcuPort(Option<String>),
    RetryCapture,
    RescanDisplays,
    Shutdown,
}

impl std::fmt::Debug for EngineControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineControl").finish_non_exhaustive()
    }
}

/// Cheap, cloneable handle for sending commands to the engine thread.
#[derive(Clone)]
pub struct EngineControl {
    tx: mpsc::Sender<Command>,
    waker: Wake,
}

impl EngineControl {
    /// Sends the command, then wakes the engine so that it reads it at once.
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
        self.waker.wake();
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine").finish_non_exhaustive()
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
    /// Starts the engine worker thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the logs folder cannot be found.
    ///
    /// # Panics
    ///
    /// Panics if the wait handle, the QPC clock, or the worker thread cannot be created.
    pub fn new(config: &Settings, repaint: Repaint) -> anyhow::Result<Self> {
        tracing::info!("spawning engine worker");
        let (tx, rx) = mpsc::channel();
        let (wait, waker) = clock::wait().expect("failed to create the engine wait");
        let (event_tx, events) = mpsc::channel();
        let qpc = Clock::new().expect("failed to read the QPC frequency");
        let (telemetry, sink) = Telemetry::spawn(qpc, repaint.clone())?;
        let state = Arc::new(Mutex::new(Arc::new(EngineState::default())));
        let links = WorkerLinks {
            commands: rx,
            events: event_tx,
            state: Arc::clone(&state),
            repaint,
            wait,
            qpc,
            telemetry: sink,
        };
        let initial = config.clone();
        let join = thread::Builder::new()
            .name("nice-aimassist-engine".to_owned())
            .spawn(move || run_worker(links, initial))
            .expect("failed to spawn engine worker");

        Ok(Self {
            control: EngineControl { tx, waker },
            events,
            state,
            telemetry,
            join: Some(join),
        })
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
