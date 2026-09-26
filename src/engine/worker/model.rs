//! Model loading.

use std::path::PathBuf;

use super::{Worker, capture::load_model_into, log_pipeline_state};
use crate::engine::{EngineEvent, ModelPhase};

impl Worker {
    /// Loads `path` on a fresh inference worker and swaps it in only on success,
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

        let candidate = self.spawn_inference().and_then(|candidate| {
            let loaded = load_model_into(
                &self.capture_worker,
                &candidate,
                &path,
                self.selected_adapter_luid,
                pipeline_id,
            )?;
            Ok((candidate, loaded))
        });
        match candidate {
            Ok((candidate, loaded)) => {
                self.inference_worker = candidate;
                self.model_ready = true;
                self.active_model = Some((name.clone(), path));
                self.metrics.mark_cold_next();
                self.commit_model_status(&loaded);
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
