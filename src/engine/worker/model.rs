//! Model loading.

use std::path::PathBuf;

use super::{Worker, log_pipeline_state};
use crate::{channel::Message, engine::ModelPhase, pipeline::PipelineConfig};

impl Worker {
    /// Loads `path` on the pipeline and keeps the previous model when the load fails.
    pub(super) fn load_model(&mut self, notify: bool, name: String, path: PathBuf) {
        let pipeline_id = self.next_pipeline_id();
        log_pipeline_state("Preparing", Some(pipeline_id), None, None, None);
        tracing::info!(model = %name, path = %path.display(), luid_low = self.selected_adapter_luid.low, luid_high = self.selected_adapter_luid.high, "loading model in engine");
        self.stop_run();
        self.edit(|state| {
            state.model.phase = ModelPhase::Loading;
            state.model.loading_name = Some(name.clone());
            state.model.error = None;
            state.run.error = None;
        });
        // Loading blocks this thread; show the state before it starts.
        self.publish();

        let config = PipelineConfig {
            model: path.as_path(),
            pipeline_id,
            confidence_threshold: self.tuning.confidence,
            low_latency: self.tuning.low_latency,
            qpc: self.clock,
        };
        match self.capture.load_model(&config) {
            Ok(()) => {
                self.active_model = Some((name.clone(), path));
                self.recorder.pipeline_changed();
                self.commit_model_status();
                tracing::info!(model = %name, "model load succeeded");
                self.edit(|state| {
                    state.model.active_name = Some(name.clone());
                    state.model.loading_name = None;
                });
                if notify {
                    self.ui.send(Message::ModelLoaded(name));
                }
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(model = %name, %message, "model load failed");
                let has_active_model = self.capture.has_model();
                self.edit(|state| {
                    state.model.phase = if has_active_model {
                        ModelPhase::Ready
                    } else {
                        ModelPhase::Empty
                    };
                    state.model.loading_name = None;
                    state.model.error = Some(message);
                });
            }
        }
    }
}
