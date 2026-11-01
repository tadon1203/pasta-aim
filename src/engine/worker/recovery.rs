//! Capture/inference faults: suspending a broken pipeline until the user retries.

use super::{Source, Worker};
use crate::engine::CapturePhase;

impl Worker {
    /// Stops the run and drops the broken GPU resources, leaving capture suspended
    /// until [`Self::retry_suspended_pipeline`] rebuilds the pipeline.
    pub(super) fn suspend_invalid_pipeline(&mut self, error: &str) {
        self.fail_run(error.to_owned());
        self.edit(|state| {
            state.capture.phase = CapturePhase::Failed;
            state.capture.selected = None;
            state.capture.desktop_size = None;
            state.capture.error = Some(error.to_owned());
        });

        // The pipeline waits for its GPU queue before its capture and COM objects are released.
        self.capture = Source::None;
        tracing::error!(%error, "invalid GPU pipeline dropped; capture remains suspended");
    }

    pub(super) fn retry_suspended_pipeline(&mut self) {
        self.stop_run();
        self.refresh_capture_catalog();
        if let Some(display) = self.selected_display() {
            self.set_capture_selection(display.adapter_index, display.output_index);
        } else {
            let error = "The previously selected capture adapter is unavailable".to_owned();
            self.edit(|state| {
                state.capture.phase = CapturePhase::Failed;
                state.capture.error = Some(error.clone());
                state.run.error = Some(error);
            });
        }
    }
}
