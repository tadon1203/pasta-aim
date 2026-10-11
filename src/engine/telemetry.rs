//! Engine-thread bookkeeping that feeds the UI and the log: the status the UI
//! reads, and the periodic continuous-run summary.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use eframe::egui;

use super::{EngineStatus, output::CommandInfo};
use crate::{graphics::GpuDropReason, metrics::PerformanceSnapshot, sync::MutexExt};

/// Upper bound on how often the UI sees status changes (and is asked to repaint).
/// The inference loop can run at several hundred Hz; the UI must not follow it.
const STATUS_PUBLISH_INTERVAL: Duration = Duration::from_millis(33);
const CONTINUOUS_LOG_INTERVAL: Duration = Duration::from_secs(1);

/// The engine's own copy of the status, published to the UI at a bounded rate.
///
/// The engine reads and edits `local` without locking; the UI only ever sees
/// snapshots copied into `shared`.
pub(super) struct StatusHub {
    local: EngineStatus,
    shared: Arc<Mutex<EngineStatus>>,
    ui_ctx: egui::Context,
    dirty: bool,
    last_publish: Instant,
}

impl StatusHub {
    pub(super) fn new(shared: Arc<Mutex<EngineStatus>>, ui_ctx: egui::Context) -> Self {
        Self {
            local: EngineStatus::default(),
            shared,
            ui_ctx,
            dirty: false,
            last_publish: Instant::now() - STATUS_PUBLISH_INTERVAL,
        }
    }

    pub(super) fn get(&self) -> &EngineStatus {
        &self.local
    }

    pub(super) fn edit(&mut self, edit: impl FnOnce(&mut EngineStatus)) {
        edit(&mut self.local);
        self.dirty = true;
    }

    /// Publishes pending edits unless one was published very recently.
    pub(super) fn flush_if_due(&mut self) {
        if self.dirty && self.last_publish.elapsed() >= STATUS_PUBLISH_INTERVAL {
            self.flush();
        }
    }

    pub(super) fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        *self.shared.lock_recover() = self.local.clone();
        self.dirty = false;
        self.last_publish = Instant::now();
        self.ui_ctx.request_repaint();
    }

    /// How long the engine loop may sleep before pending edits must be published.
    pub(super) fn time_until_flush(&self) -> Option<Duration> {
        self.dirty
            .then(|| STATUS_PUBLISH_INTERVAL.saturating_sub(self.last_publish.elapsed()))
    }
}

/// Counters for the once-per-second `continuous inference summary` log line.
pub(super) struct RunSummary {
    started: Instant,
    frames: usize,
    detections: usize,
    last_command: Option<CommandInfo>,
}

impl RunSummary {
    pub(super) fn new() -> Self {
        Self {
            started: Instant::now(),
            frames: 0,
            detections: 0,
            last_command: None,
        }
    }

    pub(super) fn reset(&mut self) {
        *self = Self::new();
    }

    pub(super) fn record(&mut self, detected: bool, command: Option<CommandInfo>) {
        self.frames += 1;
        self.detections += usize::from(detected);
        if command.is_some() {
            self.last_command = command;
        }
    }

    /// Logs and resets once per interval. `snapshot` is only built when due.
    pub(super) fn log_if_due(&mut self, snapshot: impl FnOnce() -> PerformanceSnapshot) {
        let elapsed = self.started.elapsed();
        if elapsed < CONTINUOUS_LOG_INTERVAL {
            return;
        }
        let performance = snapshot();
        let drops = &performance.gpu_drop_counts;
        tracing::debug!(
            elapsed_ms = elapsed.as_secs_f64() * 1000.0,
            frames = self.frames,
            detections = self.detections,
            last_command = ?self.last_command,
            fps = performance.fps,
            capture_p50_ms = performance.capture.p50_ms,
            inference_p50_ms = performance.inference.p50_ms,
            mak_write_p50_ms = performance.mak_write.p50_ms,
            pipeline_p50_ms = performance.total.p50_ms,
            stale_generation = drops.get(GpuDropReason::StaleGeneration),
            pipeline_poisoned = drops.get(GpuDropReason::PipelinePoisoned),
            capture_timeouts = performance.capture_source.timeouts,
            "continuous inference summary"
        );
        self.reset();
    }
}
