use std::path::Path;

use super::{CapturePhase, Message, ModelPhase, Worker};
use crate::{
    pipeline::{DisplayInfo, Options, Pipeline, displays},
    settings,
};

impl Worker {
    pub(super) fn load_model(&mut self, name: String) {
        self.stop_run();
        tracing::info!(model = %name, "loading model in engine");
        self.edit(|state| {
            state.model.phase = ModelPhase::Loading;
            state.model.loading_name = Some(name.clone());
            state.model.error = None;
            state.run.error = None;
        });
        // Loading blocks this thread; show the state before it starts.
        self.publish();

        match self.install_model(&name) {
            Ok(built) => {
                if let Some(pipeline) = built {
                    self.pipeline = Some(pipeline);
                }
                self.recorder.pipeline_changed();
                self.sync_pipeline_state();
                tracing::info!(model = %name, "model load succeeded");
                self.edit(|state| {
                    state.model.active_name = Some(name.clone());
                    state.model.loading_name = None;
                });
                self.ui.send(Message::ModelLoaded(name));
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(model = %name, %message, "model load failed");
                let phase = if self.pipeline.is_some() {
                    ModelPhase::Ready
                } else {
                    ModelPhase::Empty
                };
                self.edit(|state| {
                    state.model.phase = phase;
                    state.model.loading_name = None;
                    state.model.error = Some(message);
                });
            }
        }
    }

    fn install_model(&mut self, name: &str) -> anyhow::Result<Option<Pipeline>> {
        let path = settings::model_path(name)?;
        let options = self.options();
        match self.pipeline.as_mut() {
            Some(pipeline) => pipeline.set_model(&path, options).map(|()| None),
            None => self.build_pipeline(&self.settings.display, &path).map(Some),
        }
    }

    pub(super) fn select_display(&mut self, target: settings::Display) {
        tracing::info!(
            adapter = target.adapter,
            output = target.output,
            "changing capture selection"
        );
        self.stop_run();
        let Some(info) = self.catalog_display(&target) else {
            self.fail_capture_change("the display is not connected");
            return;
        };
        let built = match self.state.model.active_name.clone() {
            Some(name) => settings::model_path(&name)
                .and_then(|path| self.build_pipeline(&target, &path))
                .map(Some),
            None => Ok(None),
        };
        match built {
            Ok(Some(pipeline)) => {
                self.commit_pipeline(pipeline);
                self.edit(|state| state.capture.desktop_size = None);
            }
            Ok(None) => self.record_display_info(info),
            Err(error) => {
                self.fail_capture_change(&format!("{error:#}"));
                return;
            }
        }
        self.settings.display = target;
        self.ui.send(Message::DisplayChanged(target));
    }

    pub(super) fn choose_startup_display(&mut self) {
        let saved = self.settings.display;
        if self.catalog_display(&saved).is_none()
            && let Some(first) = self.state.graphics.displays.first()
        {
            let chosen = first.selection();
            tracing::info!(
                adapter = chosen.adapter,
                output = chosen.output,
                "saved display unavailable; selected the first display"
            );
            self.settings.display = chosen;
            self.ui.send(Message::DisplayChanged(chosen));
        }
        self.record_display(self.settings.display);
    }

    fn record_display(&mut self, display: settings::Display) {
        match self.catalog_display(&display) {
            Some(info) => self.record_display_info(info),
            None => self.edit(|state| {
                state.capture.phase = CapturePhase::Failed;
                state.capture.selected = None;
                state.capture.error = Some("The selected display is unavailable".to_owned());
            }),
        }
    }

    fn record_display_info(&mut self, info: DisplayInfo) {
        self.edit(|state| {
            state.capture.phase = CapturePhase::Ready;
            state.capture.selected = Some(info);
            state.capture.error = None;
        });
    }

    fn fail_capture_change(&mut self, reason: &str) {
        let message = format!("Display change failed: {reason}");
        tracing::error!(%message, "display change failed; active pipeline retained");
        let phase = if self.pipeline.is_some() {
            CapturePhase::Ready
        } else {
            CapturePhase::Failed
        };
        self.edit(|state| {
            state.capture.phase = phase;
            state.capture.error = Some(message);
        });
    }

    pub(super) fn retry_capture(&mut self) {
        self.stop_run();
        self.refresh_catalog();
        self.select_display(self.settings.display);
    }

    pub(super) fn refresh_catalog(&mut self) {
        if !self.com_ready {
            return;
        }
        match displays() {
            Ok(found) => self.edit(|state| {
                state.graphics.displays = found;
                state.graphics.catalog_error = None;
            }),
            Err(error) => {
                let error = format!("{error:#}");
                tracing::error!(%error, "graphics enumeration failed");
                self.edit(|state| {
                    state.graphics.displays.clear();
                    state.graphics.catalog_error = Some(error);
                });
            }
        }
    }

    fn catalog_display(&self, display: &settings::Display) -> Option<DisplayInfo> {
        self.state
            .graphics
            .displays
            .iter()
            .find(|info| info.selection() == *display)
            .cloned()
    }

    fn options(&self) -> Options {
        Options {
            confidence: self.settings.confidence,
            low_latency: self.settings.low_latency,
        }
    }

    pub(super) fn build_pipeline(
        &self,
        display: &settings::Display,
        model: &Path,
    ) -> anyhow::Result<Pipeline> {
        anyhow::ensure!(self.com_ready, "graphics COM initialization unavailable");
        Pipeline::new(display, model, self.options())
    }

    /// The old pipeline is dropped only after the new one is in place.
    pub(super) fn commit_pipeline(&mut self, pipeline: Pipeline) {
        self.pipeline = Some(pipeline);
        self.recorder.pipeline_changed();
        self.sync_pipeline_state();
    }

    fn sync_pipeline_state(&mut self) {
        let Some(pipeline) = self.pipeline.as_ref() else {
            return;
        };
        let selected = pipeline.display().clone();
        let input_size = pipeline.input_size();
        let precision = pipeline.precision();
        let cpu_nodes = pipeline.cpu_nodes().cloned();
        let graph_capture = pipeline.graph_capture();
        self.edit(|state| {
            state.capture.phase = CapturePhase::Ready;
            state.capture.selected = Some(selected);
            state.capture.error = None;
            state.model.phase = ModelPhase::Ready;
            state.model.input_size = Some(input_size);
            state.model.precision = Some(precision);
            state.model.cpu_nodes = cpu_nodes;
            state.model.graph_capture = Some(graph_capture);
            state.model.error = None;
        });
    }
}
