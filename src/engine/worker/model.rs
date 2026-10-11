//! Model loading.

use std::path::PathBuf;

use super::{Worker, log_pipeline_state};
use crate::{
    engine::{EngineEvent, ModelPhase},
    pipeline::PipelineConfig,
};

impl Worker {
    /// Loads `path` on the Pipeline and keeps the previous model when the load fails.
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

        let config = PipelineConfig {
            model: path.as_path(),
            pipeline_id,
            confidence_threshold: self.tuning.confidence_threshold,
            low_latency: self.tuning.low_latency,
            qpc: self.qpc,
        };
        match self.capture.load_model(config) {
            Ok(()) => {
                self.active_model = Some((name.clone(), path));
                self.telemetry.pipeline_changed();
                self.commit_model_status();
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
                let has_active_model = self.capture.has_model();
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
