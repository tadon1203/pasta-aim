//! Starting, stopping, and observing inference runs.

use super::Worker;
use crate::{
    engine::{
        RunGeneration, RunMode, RunPhase,
        status::{RunReadiness, run_block_reason},
    },
    telemetry::{RunCounters, RunStart, TraceHeader},
};

#[derive(Debug, Clone, Copy)]
pub(super) struct ActiveRun {
    pub generation: RunGeneration,
    pub mode: RunMode,
}

#[derive(Default)]
pub(super) struct RunControl {
    last_generation: RunGeneration,
    pub active: Option<ActiveRun>,
}

impl RunControl {
    pub(super) fn next_generation(&mut self) -> RunGeneration {
        self.last_generation = self.last_generation.wrapping_add(1).max(1);
        self.last_generation
    }

    pub(super) fn is_active(&self, generation: RunGeneration) -> bool {
        self.active.is_some_and(|run| run.generation == generation)
    }

    pub(super) fn is_continuous(&self) -> bool {
        self.active
            .is_some_and(|run| run.mode == RunMode::Continuous)
    }
}

impl Worker {
    pub(super) fn set_continuous(&mut self, active: bool) {
        if !active {
            self.stop_run();
        } else if !self.run.is_continuous() {
            self.start_run(RunMode::Continuous);
        }
    }

    pub(super) fn run_once(&mut self) {
        if self.run.is_continuous() {
            tracing::warn!("single inference requested while continuous inference is active");
            self.set_run_error("Stop continuous inference before running once".to_owned());
            return;
        }
        self.start_run(RunMode::OneShot);
    }

    pub(super) fn start_run(&mut self, mode: RunMode) {
        if let Some(reason) = self.run_block_reason() {
            tracing::warn!(%reason, ?mode, "inference start blocked");
            self.set_run_error(reason);
            return;
        }
        let generation = self.run.next_generation();
        self.output_stage.reset();
        self.capture_deadline.clear();
        self.run_counters = RunCounters::default();
        self.sent_counters = RunCounters::default();
        if let Some(capture) = self.capture.as_mut() {
            capture.reset_gpu_profiler();
        }
        self.run.active = Some(ActiveRun { generation, mode });
        let phase = match mode {
            RunMode::Continuous => {
                tracing::info!(
                    fps_limit = self.tuning.fps_limit.value(),
                    "continuous inference started"
                );
                RunPhase::Continuous
            }
            RunMode::OneShot => RunPhase::OneShot,
        };
        self.telemetry.run_started(RunStart {
            generation,
            mode,
            trace: self.trace_header(),
        });
        self.status.edit(|status| {
            status.run.phase = phase;
            status.run.error = None;
        });
    }

    /// The header of the Frame trace, when the trace is on for this Run.
    fn trace_header(&self) -> Option<TraceHeader> {
        if !self.tuning.frame_trace {
            return None;
        }
        let output = self.tuning.output;
        Some(TraceHeader {
            qpc_hz: self.qpc.frequency_hz(),
            input_size: self.model.as_ref().map(|model| model.input_size),
            sensitivity_percent: output.sensitivity.value(),
            target_y_offset_percent: output.target_y_offset.value(),
            max_delta: output.max_delta.value(),
        })
    }

    /// Stops any active run. Safe to call when idle.
    pub(super) fn stop_run(&mut self) {
        if let Some(run) = self.run.active.take() {
            tracing::info!(generation = run.generation, "inference stopped");
            self.telemetry.run_stopped(run.generation);
        }
        self.output_stage.reset();
        if self.status.get().run.phase != RunPhase::Idle {
            self.status.edit(|status| status.run.phase = RunPhase::Idle);
        }
    }

    pub(super) fn fail_run(&mut self, error: String) {
        tracing::error!(%error, "inference stopped after error");
        self.stop_run();
        self.set_run_error(error);
    }

    pub(super) fn run_block_reason(&self) -> Option<String> {
        let makcu = &self.makcu;
        run_block_reason(RunReadiness {
            model_ready: self.model_ready,
            capture_ready: self.capture_ready,
            makcu_connected: makcu.is_connected(),
            mouse_route: makcu.mouse_available(),
        })
        .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_generation_is_rejected() {
        let mut run = RunControl::default();
        let generation = run.next_generation();
        assert!(!run.is_active(generation));
        run.active = Some(ActiveRun {
            generation,
            mode: RunMode::Continuous,
        });
        assert!(run.is_active(generation));
        assert!(!run.is_active(generation + 1));
        assert!(run.is_continuous());
    }

    #[test]
    fn generations_are_never_zero() {
        let mut run = RunControl {
            last_generation: RunGeneration::MAX,
            active: None,
        };
        assert_eq!(run.next_generation(), 1);
    }
}
