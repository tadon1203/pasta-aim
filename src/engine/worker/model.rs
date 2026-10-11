//! Model loading.

use std::path::PathBuf;

use super::{Worker, capture::prepare_model, log_pipeline_state};
use crate::engine::{EngineEvent, ModelPhase};

impl Worker {
    /// Loads `path` on a candidate GPU pipeline and commits it only on success,
    /// so a failed load keeps the previous model running.
    pub(super) fn load_model(&mut self, notify: bool, name: String, path: PathBuf) {
        self.auto_reprepare_attempted = false;
        let pipeline_id = self.next_pipeline_id();
        log_pipeline_state("Preparing", Some(pipeline_id), None, None, None);
        tracing::info!(model = %name, path = %path.display(), luid_low = self.selected_adapter_luid.low, luid_high = self.selected_adapter_luid.high, "loading model in engine");
        self.stop_run();
        self.status.edit(|status| {
            status.model.phase = ModelPhase::Loading;
            status.model.loading_name = Some(name.clone());
            status.model.error = None;
            status.run.error = None;
        });
        // Loading blocks this thread; show the state before it starts.
        self.status.flush();

        let candidate = self
            .capture
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("capture is not configured"))
            .and_then(|capture| prepare_model(capture, &path, pipeline_id, self.tuning));
        match candidate {
            Ok(loaded) => {
                self.model_ready = true;
                self.active_model = Some((name.clone(), path));
                self.telemetry.pipeline_changed();
                self.commit_model_status(&loaded);
                self.model = Some(loaded);
                self.status.edit(|status| {
                    status.model.active_name = Some(name.clone());
                    status.model.loading_name = None;
                });
                if notify {
                    self.emit_event(EngineEvent::Model {
                        name: name.clone(),
                        result: Ok(()),
                    });
                }
                tracing::info!(model = %name, "model load succeeded");
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(model = %name, %message, "model load failed");
                let has_active_model = self.model_ready;
                self.status.edit(|status| {
                    status.model.phase = if has_active_model {
                        ModelPhase::Ready
                    } else {
                        ModelPhase::Empty
                    };
                    status.model.loading_name = None;
                    status.model.error = Some(message.clone());
                });
                if notify {
                    self.emit_event(EngineEvent::Model {
                        name,
                        result: Err(message),
                    });
                }
            }
        }
    }
}
