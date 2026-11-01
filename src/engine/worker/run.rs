//! Starting, stopping, and observing inference runs.

use std::time::Instant;

use super::Worker;
use crate::engine::{
    RunPhase,
    state::{BLOCK_CAPTURE, BLOCK_MODEL, BLOCK_MOUSE_ROUTE, BLOCK_OUTPUT, run_block_reason},
};

/// The active run. `started` is when it began: a frame whose capture was attempted before that is stale.
#[derive(Debug, Clone, Copy)]
pub(super) struct ActiveRun {
    /// `OneShot` or `Continuous`.
    pub mode: RunPhase,
    pub started: Instant,
}

#[derive(Default)]
pub(super) struct RunControl {
    pub active: Option<ActiveRun>,
}

impl RunControl {
    pub(super) fn is_continuous(&self) -> bool {
        self.active
            .is_some_and(|run| run.mode == RunPhase::Continuous)
    }
}

impl Worker {
    pub(super) fn set_continuous(&mut self, active: bool) {
        if !active {
            self.stop_run();
        } else if !self.run.is_continuous() {
            self.start_run(RunPhase::Continuous);
        }
    }

    pub(super) fn run_once(&mut self) {
        if self.run.is_continuous() {
            tracing::warn!("single inference requested while continuous inference is active");
            self.set_run_error("Stop continuous inference before running once".to_owned());
            return;
        }
        self.start_run(RunPhase::OneShot);
    }

    pub(super) fn start_run(&mut self, mode: RunPhase) {
        if let Some(reason) = self.run_block_reason() {
            tracing::warn!(%reason, ?mode, "inference start blocked");
            self.set_run_error(reason);
            return;
        }
        self.begin_run(mode);
    }

    /// Starts a run with no readiness check. `start_run` checks the readiness first.
    pub(super) fn begin_run(&mut self, mode: RunPhase) {
        self.aim.reset();
        self.capture_deadline.clear();
        self.recorder.run_started(Instant::now());
        if let Some(pipeline) = self.capture.pipeline_mut() {
            pipeline.reset_profiling();
        }
        self.run.active = Some(ActiveRun {
            mode,
            started: Instant::now(),
        });
        if mode == RunPhase::Continuous {
            tracing::info!(
                fps_limit = self.tuning.fps_limit.get(),
                "continuous inference started"
            );
        }
        self.edit(|state| {
            state.run.phase = mode;
            state.run.error = None;
        });
    }

    /// Stops any active run. Safe to call when idle.
    pub(super) fn stop_run(&mut self) {
        if self.run.active.take().is_some() {
            tracing::info!("inference stopped");
        }
        self.aim.reset();
        if self.state.run.phase != RunPhase::Idle {
            self.edit(|state| state.run.phase = RunPhase::Idle);
        }
    }

    pub(super) fn fail_run(&mut self, error: String) {
        tracing::error!(%error, "inference stopped after error");
        self.stop_run();
        self.set_run_error(error);
    }

    pub(super) fn run_block_reason(&self) -> Option<String> {
        run_block_reason([
            (self.capture.has_model(), BLOCK_MODEL),
            (self.capture.is_capture_ready(), BLOCK_CAPTURE),
            (self.output.is_connected(), BLOCK_OUTPUT),
            (self.output.mouse_route(), BLOCK_MOUSE_ROUTE),
        ])
        .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_is_the_mode_of_the_active_run() {
        let mut run = RunControl::default();
        assert!(!run.is_continuous());
        run.active = Some(ActiveRun {
            mode: RunPhase::Continuous,
            started: Instant::now(),
        });
        assert!(run.is_continuous());
        run.active = Some(ActiveRun {
            mode: RunPhase::OneShot,
            started: Instant::now(),
        });
        assert!(!run.is_continuous());
    }
}
