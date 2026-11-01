mod state;
mod stats;
mod worker;

pub use state::{CapturePhase, ModelPhase, OutputPhase, RunPhase, State};
pub use stats::{Percentiles, Rates, Stages, Stats};

use std::{
    path::PathBuf,
    thread::{self, JoinHandle},
};

use crate::{
    channel::{Message, Sender, channel},
    clock::{self, Clock},
    settings::{
        Confidence, FpsLimit, Interpolation, MaxDelta, NewFrameWait, Sensitivity, Settings,
        TargetYOffset,
    },
};

use worker::{WorkerLinks, run_worker};

/// Requests accepted by the engine thread.
#[derive(Debug)]
pub enum Command {
    /// `notify` reports the success to the UI (which then persists the choice).
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
    SetCaptureSelection {
        adapter: u32,
        output: u32,
    },
    SetContinuous(bool),
    RunOnce,
    /// Connects again: the output ports in order, or only the one that the user chose.
    ReconnectOutput,
    /// Chooses the output port (`None` is auto-detect) and connects again.
    SetOutputPort(Option<String>),
    RetryCapture,
    RescanDisplays,
    Shutdown,
}

/// The engine thread. Dropping it stops the thread and waits for it.
#[derive(Debug)]
pub struct Engine {
    commands: Sender<Command>,
    join: Option<JoinHandle<()>>,
}

impl Engine {
    /// Starts the engine thread. It sends the state and the statistics to `ui`.
    ///
    /// # Panics
    ///
    /// Panics if the wait handle, the QPC clock, or the thread cannot be created.
    pub fn spawn(settings: &Settings, ui: Sender<Message>) -> Self {
        tracing::info!("spawning engine thread");
        let (wait, wake) = clock::wait().expect("failed to create the engine wait");
        let (commands, inbox) = channel(move || wake.wake());
        let links = WorkerLinks {
            commands: inbox,
            ui,
            wait,
            clock: Clock::new().expect("failed to read the QPC frequency"),
        };
        let settings = settings.clone();
        let join = thread::Builder::new()
            .name("nice-aimassist-engine".to_owned())
            .spawn(move || run_worker(links, settings))
            .expect("failed to spawn the engine thread");
        Self {
            commands,
            join: Some(join),
        }
    }

    pub fn sender(&self) -> &Sender<Command> {
        &self.commands
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        tracing::info!("shutting down engine thread");
        self.commands.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
