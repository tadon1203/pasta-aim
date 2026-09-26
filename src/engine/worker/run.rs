//! Starting, stopping, and observing inference runs.

use super::Worker;
use crate::{
    engine::CapturePhase,
    engine::{
        RunGeneration, RunMode, RunPhase,
        inference_worker::{FrameReport, InferenceEvent},
        status::{RunReadiness, run_block_reason},
    },
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
        self.gate.reset();
        let started = self
            .capture_worker
            .start(mode, generation)
            .and_then(|()| self.inference_worker.start(generation, mode));
        if let Err(error) = started {
            let _ = self.inference_worker.stop(generation);
            let _ = self.capture_worker.stop();
            self.run.active = None;
            self.mak().release_controller_best_effort();
            let label = match mode {
                RunMode::Continuous => "Failed to start inference",
                RunMode::OneShot => "Single inference failed",
            };
            tracing::error!(error = %format!("{error:#}"), ?mode, "inference failed to start");
            self.status.edit(|status| {
                status.run.phase = RunPhase::Idle;
                status.run.error = Some(format!("{label}: {error:#}"));
            });
            return;
        }
        self.run.active = Some(ActiveRun { generation, mode });
        let phase = match mode {
            RunMode::Continuous => {
                self.summary.reset();
                tracing::info!(
                    fps_limit = self.tuning.fps_limit.value(),
                    "continuous inference started"
                );
                RunPhase::Continuous
            }
            RunMode::OneShot => RunPhase::OneShot,
        };
        self.status.edit(|status| {
            status.run.phase = phase;
            status.run.error = None;
        });
    }

    /// Stops any active run and releases the controller. Safe to call when idle.
    pub(super) fn stop_run(&mut self) {
        self.stop_run_inner(true);
    }

    /// `clear_overlay = false` lets the last box of a finished one-shot run
    /// stay visible until it expires.
    fn stop_run_inner(&mut self, clear_overlay: bool) {
        if let Some(run) = self.run.active.take() {
            tracing::info!(generation = run.generation, "inference stopped");
            // The inference thread acknowledges the stop before anything is
            // released, so it cannot submit output after the release below.
            let _ = self.inference_worker.stop(run.generation);
            let _ = self.capture_worker.stop();
        }
        // After the inference thread acknowledged the stop, so nothing can
        // publish a box behind this.
        if clear_overlay && let Some(overlay) = &self.overlay {
            overlay.clear();
        }
        self.mak().release_controller_best_effort();
        let pending = self.mak().controller_release_pending();
        self.status.edit(|status| {
            status.run.phase = RunPhase::Idle;
            status.mak.controller_release_pending = pending;
        });
    }

    pub(super) fn fail_run(&mut self, error: String) {
        tracing::error!(%error, "inference stopped after error");
        self.stop_run();
        self.set_run_error(error);
    }

    pub(super) fn run_block_reason(&self) -> Option<String> {
        let mak = self.mak();
        run_block_reason(
            RunReadiness {
                model_ready: self.model_ready,
                capture_ready: self.status.get().capture.phase == CapturePhase::Ready,
                mak_connected: mak.is_connected(),
                mouse_route: mak.mouse_available(),
                controller_route: mak.controller_available(),
            },
            self.tuning.output.mode,
        )
        .map(str::to_owned)
    }

    pub(super) fn handle_inference_event(&mut self, event: InferenceEvent) {
        match event {
            InferenceEvent::Frame(report) => self.handle_frame(*report),
            InferenceEvent::Failed { generation, error } => {
                if self.run.is_active(generation) {
                    self.suspend_invalid_pipeline(error);
                }
            }
            InferenceEvent::Poisoned {
                generation,
                error,
                device_lost,
                report,
            } => {
                let belongs_to_active_pipeline = report
                    .as_ref()
                    .is_some_and(|report| Some(report.pipeline_id) == self.active_pipeline_id);
                if self.run.is_active(generation) || belongs_to_active_pipeline {
                    self.recover_poisoned_pipeline(error, device_lost, report.map(|r| *r));
                }
            }
            InferenceEvent::OutputFailed { generation, error } => {
                if self.run.is_active(generation) {
                    self.fail_run(format!("Inference output failed: {error}"));
                }
            }
        }
    }

    fn handle_frame(&mut self, report: FrameReport) {
        if !self.run.is_active(report.generation) {
            tracing::debug!(
                generation = report.generation,
                "discarding old-generation inference report"
            );
            return;
        }
        self.metrics
            .record_qpc(self.qpc, report.inference_start_qpc, report.timings);
        self.drain_gate_discards();
        self.summary.record(report.detected, report.command);
        let performance = self.metrics.snapshot_if_due(report.one_shot_done);
        self.status.edit(|status| {
            status.capture.desktop_size = Some(report.metadata.desktop_size);
            status.run.last_target = report.target;
            status.run.last_command = report.command;
            status.run.last_total_ms = Some(report.timings.total_ms);
            status.run.error = None;
            status.mak.physical_controller = report.mak.physical_controller;
            status.mak.controller_release_pending = report.mak.controller_release_pending;
            if let Some(performance) = performance {
                status.performance = performance;
            }
        });
        if report.one_shot_done {
            self.stop_run_inner(false);
        } else {
            let metrics = &self.metrics;
            self.summary.log_if_due(|| metrics.snapshot());
        }
    }

    /// Moves causal-gate discards (counted on the capture and inference threads) into metrics.
    pub(super) fn drain_gate_discards(&mut self) {
        let discards = self.gate.take_discards();
        if discards > 0 {
            self.metrics.record_causal_discards(discards);
        }
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
