//! Capture/inference faults: suspending a broken pipeline until the user retries.

use super::{CaptureState, Worker};
use crate::engine::CapturePhase;

impl Worker {
    /// Stops the run and drops the broken GPU resources, leaving capture suspended
    /// until [`Self::retry_suspended_pipeline`] rebuilds the pipeline.
    pub(super) fn suspend_invalid_pipeline(&mut self, error: &str) {
        self.fail_run(error.to_owned());
        self.status.edit(|status| {
            status.capture.phase = CapturePhase::Failed;
            status.capture.selected = None;
            status.capture.desktop_size = None;
            status.capture.error = Some(error.to_owned());
        });

        // The pipeline waits for its GPU queue before its capture and COM objects are released.
        self.capture = CaptureState::None;
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
