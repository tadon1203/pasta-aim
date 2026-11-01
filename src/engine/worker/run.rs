//! Starting, stopping, and observing inference runs.

use std::time::{SystemTime, UNIX_EPOCH};

use super::Worker;
use crate::{
    clock::Clock,
    engine::{
        MakcuPhase, RunGeneration, RunMode, RunPhase,
        status::{BLOCK_CAPTURE, BLOCK_MAKCU, BLOCK_MODEL, BLOCK_MOUSE_ROUTE, run_block_reason},
    },
    makcu::format_firmware,
    telemetry::{
        AppInfo, ClockInfo, MakcuInfo, ModelInfo, RunCounters, RunInfo, RunStart, SettingsInfo,
        TraceHeader,
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
        self.begin_run(mode);
    }

    /// Starts a run with no readiness check. `start_run` checks the readiness first.
    pub(super) fn begin_run(&mut self, mode: RunMode) {
        let generation = self.run.next_generation();
        self.output_stage.reset();
        self.capture_deadline.clear();
        self.run_counters = RunCounters::default();
        self.sent_counters = RunCounters::default();
        if let Some(pipeline) = self.capture.pipeline_mut() {
            pipeline.reset_profiling();
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
            trace: self.trace_header(generation, mode),
        });
        self.status.edit(|status| {
            status.run.phase = phase;
            status.run.error = None;
        });
    }

    /// The header of the frame trace, when the trace is on for this run.
    fn trace_header(&self, generation: RunGeneration, mode: RunMode) -> Option<TraceHeader> {
        if !self.tuning.frame_trace {
            return None;
        }
        let tuning = &self.tuning;
        let makcu = self.status.get().makcu.clone();
        let makcu = match (makcu.phase, makcu.port, makcu.firmware) {
            (MakcuPhase::Connected, Some(port), Some(firmware)) => Some(MakcuInfo {
                port,
                firmware: format_firmware(firmware),
                baud: makcu.baud,
                interpolation: self.makcu.interpolation(),
            }),
            _ => None,
        };
        Some(TraceHeader {
            app: AppInfo {
                version: env!("CARGO_PKG_VERSION"),
                build: if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "release"
                },
            },
            clock: ClockInfo {
                qpc_hz: self.qpc.frequency(),
                run_start_qpc: Clock::now(),
                run_start_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_millis() as u64),
            },
            run: RunInfo {
                generation,
                mode: match mode {
                    RunMode::Continuous => "continuous",
                    RunMode::OneShot => "one_shot",
                },
            },
            makcu,
            model: ModelInfo {
                name: self.active_model.as_ref().map(|(name, _)| name.clone()),
                input_size: self
                    .capture
                    .pipeline()
                    .map(crate::pipeline::Pipeline::input_size),
            },
            settings: SettingsInfo {
                fps_limit: tuning.fps_limit.value(),
                capture_wait_ms: tuning.capture.wait_ms,
                low_latency: tuning.low_latency,
                confidence_threshold_percent: tuning.confidence_threshold.value(),
                sensitivity_percent: tuning.output.sensitivity.value(),
                target_y_offset_percent: tuning.output.target_y_offset.value(),
                max_delta: tuning.output.max_delta.value(),
            },
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
        run_block_reason([
            (self.capture.has_model(), BLOCK_MODEL),
            (self.capture.is_capture_ready(), BLOCK_CAPTURE),
            (makcu.is_connected(), BLOCK_MAKCU),
            (makcu.mouse_available(), BLOCK_MOUSE_ROUTE),
        ])
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
