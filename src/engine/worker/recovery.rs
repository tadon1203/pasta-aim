//! Capture/inference faults: suspending a broken pipeline and rebuilding it.

use super::{Worker, log_pipeline_state, spawn_workers};
use crate::{
    engine::{
        CapturePhase,
        capture_worker::{CaptureEvent, CaptureFault},
    },
    graphics::{GpuDropReason, GpuFaultReport},
};

impl Worker {
    pub(super) fn handle_capture_event(&mut self, event: CaptureEvent) {
        match event {
            CaptureEvent::Fault(fault) => self.handle_capture_fault(*fault),
            CaptureEvent::Stats(value) => self.metrics.update_capture_source(value),
            CaptureEvent::GpuCopy(sample) => self.metrics.record_gpu_copy(sample.gpu_copy_ms),
        }
        if let Some(performance) = self.metrics.snapshot_if_due(false) {
            self.status.edit(|status| status.performance = performance);
        }
    }

    fn handle_capture_fault(&mut self, fault: CaptureFault) {
        if let Some(generation) = fault.generation
            && !self.run.is_active(generation)
            && !fault
                .report
                .as_ref()
                .is_some_and(|report| Some(report.pipeline_id) == self.active_pipeline_id)
        {
            tracing::debug!(generation, "discarding stale capture fault");
            return;
        }
        if fault.poisoned {
            self.recover_poisoned_pipeline(
                format!("Capture transport failed: {}", fault.error),
                fault.device_lost,
                fault.report,
            );
        } else {
            self.suspend_invalid_pipeline(format!("Capture failed: {}", fault.error));
        }
    }

    /// Tears the pipeline down after a poisoned GPU transport and rebuilds it once.
    pub(super) fn recover_poisoned_pipeline(
        &mut self,
        error: String,
        device_lost: bool,
        report: Option<GpuFaultReport>,
    ) {
        let resume_continuous = self.run.is_continuous();
        self.metrics
            .record_gpu_drop(GpuDropReason::PipelinePoisoned);
        let (reported_pipeline_id, prepare_id) = log_fault_report(report);
        let pipeline_id = reported_pipeline_id.or(self.active_pipeline_id);
        let prepare_id = prepare_id.as_deref();
        log_pipeline_state("Poisoned", pipeline_id, prepare_id, None, None);
        let can_attempt_reprepare = should_auto_reprepare(&mut self.auto_reprepare_attempted);
        log_pipeline_state("Teardown", pipeline_id, prepare_id, None, None);
        self.suspend_invalid_pipeline(error.clone());
        if !can_attempt_reprepare {
            let reason = if device_lost {
                "repeated_device_removed_after_single_reprepare"
            } else {
                "repeated_fatal_gpu_fault_after_single_reprepare"
            };
            log_pipeline_state(
                "GpuPipelineInvalid",
                pipeline_id,
                prepare_id,
                Some(1),
                Some(reason),
            );
            self.set_run_error(format!(
                "{error}; GPU pipeline invalid after its single automatic reprepare"
            ));
            return;
        }

        log_pipeline_state("Preparing", pipeline_id, prepare_id, Some(1), None);
        tracing::warn!(%error, "repreparing complete GPU pipeline once after transport poison");
        self.retry_suspended_pipeline();
        if resume_continuous && self.model_ready {
            self.set_continuous(true);
        }
        if !self.model_ready {
            log_pipeline_state(
                "GpuPipelineInvalid",
                pipeline_id,
                prepare_id,
                Some(1),
                Some("automatic_reprepare_failed"),
            );
        }
    }

    /// Stops the run and drops the broken workers, leaving capture suspended
    /// until [`Self::retry_suspended_pipeline`] rebuilds them.
    pub(super) fn suspend_invalid_pipeline(&mut self, error: String) {
        self.fail_run(error.clone());
        self.model_ready = false;
        self.status.edit(|status| {
            status.capture.phase = CapturePhase::Failed;
            status.capture.selected = None;
            status.capture.desktop_size = None;
            status.capture.error = Some(error.clone());
        });

        // Stop and join both owners before constructing replacement devices.
        self.inference_worker.shutdown();
        self.capture_worker.shutdown();
        match spawn_workers(self.qpc, self.tuning, &self.mak, &self.gate) {
            Ok((capture, inference, consumer)) => {
                self.inference_worker = inference;
                self.capture_worker = capture;
                self.frame_consumer = consumer;
            }
            Err(rebuild_error) => {
                tracing::error!(error = %format!("{rebuild_error:#}"), "failed to create suspended pipeline workers");
            }
        }
        tracing::error!(%error, "invalid GPU pipeline dropped; capture remains suspended");
    }

    pub(super) fn retry_suspended_pipeline(&mut self) {
        self.stop_run();
        self.refresh_capture_catalog();
        if let Some(display) = self.selected_display() {
            self.set_capture_selection(display.adapter_index, display.output_index);
        } else {
            let error = "The previously selected capture adapter is unavailable".to_owned();
            self.status.edit(|status| {
                status.capture.phase = CapturePhase::Failed;
                status.capture.error = Some(error.clone());
                status.run.error = Some(error);
            });
        }
    }
}

/// Logs a fault report, returning the pipeline it belongs to.
fn log_fault_report(report: Option<GpuFaultReport>) -> (Option<u64>, Option<String>) {
    let Some(report) = report else {
        return (None, None);
    };
    report.log();
    (Some(report.pipeline_id), Some(report.prepare_id))
}

fn should_auto_reprepare(attempted: &mut bool) -> bool {
    !std::mem::replace(attempted, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_full_reprepare_is_allowed_once_only() {
        let mut attempted = false;
        assert!(should_auto_reprepare(&mut attempted));
        assert!(attempted);
        assert!(!should_auto_reprepare(&mut attempted));
    }
}
