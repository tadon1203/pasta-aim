//! Display catalog, capture selection, and building the Pipeline of the selected display.

use super::{CaptureState, Worker};
use crate::{
    engine::{CapturePhase, EngineEvent, ModelPhase, RunMode},
    graphics::{DisplayInfo, DxgiCapture, GraphicsThread},
    pipeline::{Pipeline, PipelineConfig},
};
use anyhow::Result;

impl CaptureState {
    /// Loads a model on this state. A bare capture gets a Pipeline; a Pipeline gets its model replaced.
    /// On failure the state keeps the capture, or the Pipeline with the model it had.
    pub(super) fn load_model(&mut self, config: &PipelineConfig<'_>) -> Result<()> {
        match std::mem::replace(self, CaptureState::None) {
            CaptureState::None => Err(anyhow::anyhow!("capture is not configured")),
            CaptureState::CaptureOnly(capture) => match Pipeline::build(*capture, config) {
                Ok(pipeline) => {
                    *self = CaptureState::Ready(Box::new(pipeline));
                    Ok(())
                }
                Err((capture, error)) => {
                    *self = CaptureState::CaptureOnly(Box::new(capture));
                    Err(error)
                }
            },
            CaptureState::Ready(mut pipeline) => {
                let result = pipeline.replace_model(config);
                *self = CaptureState::Ready(pipeline);
                result
            }
        }
    }
}

impl Worker {
    pub(super) fn refresh_capture_catalog(&mut self) {
        if self.graphics.is_none() {
            return;
        }
        match GraphicsThread::enumerate_displays() {
            Ok(displays) => {
                self.displays.clone_from(&displays);
                self.status.edit(|status| {
                    status.graphics.displays = displays;
                    status.graphics.catalog_error = None;
                });
            }
            Err(error) => {
                let error = format!("{error:#}");
                tracing::error!(%error, "graphics enumeration failed");
                self.displays.clear();
                self.status.edit(|status| {
                    status.graphics.displays.clear();
                    status.graphics.catalog_error = Some(error);
                });
            }
        }
    }

    /// The catalog entry for the remembered adapter and output, if it is present.
    pub(super) fn selected_display(&self) -> Option<DisplayInfo> {
        self.displays
            .iter()
            .find(|display| {
                display.adapter_luid == self.selected_adapter_luid
                    && display.output_index == self.capture_output
            })
            .cloned()
    }

    pub(super) fn initialize_capture(&mut self) {
        if let Some(display) = self.selected_display() {
            self.capture_adapter = display.adapter_index;
        }
        tracing::info!(
            adapter = self.capture_adapter,
            output = self.capture_output,
            "initializing selected capture"
        );
        self.status.edit(|status| {
            status.capture.phase = CapturePhase::Initializing;
            status.capture.error = None;
            status.capture.desktop_size = None;
        });
        self.status.flush();

        match self.create_capture(self.capture_adapter, self.capture_output) {
            Ok(capture) => {
                let selected = capture.info().clone();
                self.capture = CaptureState::CaptureOnly(Box::new(capture));
                self.selected_adapter_luid = selected.adapter_luid;
                self.status.edit(|status| {
                    status.capture.phase = CapturePhase::Ready;
                    status.capture.selected = Some(selected);
                    status.capture.error = None;
                });
                tracing::info!("selected capture initialized");
            }
            Err(error) => {
                tracing::error!(error = %crate::logging::chain(&error), "selected capture initialization failed");
                self.status.edit(|status| {
                    status.capture.phase = CapturePhase::Failed;
                    status.capture.selected = None;
                    status.capture.error = Some(format!("{error:#}"));
                });
            }
        }
    }

    pub(super) fn set_capture_selection(&mut self, adapter: u32, output: u32) {
        tracing::info!(adapter, output, "changing capture selection");
        let current = self.capture.display().cloned();
        if let Some(selected) = &current
            && selected.adapter_index == adapter
            && selected.output_index == output
        {
            self.emit_event(EngineEvent::Capture {
                adapter,
                output,
                adapter_luid: selected.adapter_luid,
                result: Ok(()),
            });
            return;
        }
        let resume_continuous = self.run.is_continuous();
        self.stop_run();
        let pipeline_id = self.next_pipeline_id();
        // Build the candidate before the state is replaced, so that a failure keeps the state.
        let candidate = self.build_candidate(adapter, output, pipeline_id);
        let state = match candidate {
            Ok(state) => state,
            Err(error) => {
                let message = format!("Display change failed: {error:#}");
                let phase = if self.capture.is_capture_ready() {
                    CapturePhase::Ready
                } else {
                    CapturePhase::Failed
                };
                self.status.edit(|status| {
                    status.capture.phase = phase;
                    status.capture.error = Some(message.clone());
                });
                self.emit_event(EngineEvent::Capture {
                    adapter,
                    output,
                    adapter_luid: current
                        .as_ref()
                        .map_or(self.selected_adapter_luid, |display| display.adapter_luid),
                    result: Err(message.clone()),
                });
                tracing::error!(adapter, output, %message, "display prepare failed; active pipeline retained");
                // The Run was active, so its readiness held. This change left the state as it was.
                if resume_continuous {
                    self.begin_run(RunMode::Continuous);
                }
                return;
            }
        };

        let selected = state
            .display()
            .cloned()
            .expect("a built capture state has a display");
        self.capture_adapter = adapter;
        self.capture_output = output;
        self.selected_adapter_luid = selected.adapter_luid;
        self.telemetry.pipeline_changed();
        // The old state is dropped after the new one is in place.
        self.capture = state;
        self.commit_model_status();
        self.status.edit(|status| {
            status.capture.phase = CapturePhase::Ready;
            status.capture.selected = Some(selected.clone());
            status.capture.desktop_size = None;
            status.capture.error = None;
            status.run.error = None;
        });
        self.emit_event(EngineEvent::Capture {
            adapter,
            output,
            adapter_luid: selected.adapter_luid,
            result: Ok(()),
        });
        if resume_continuous && self.capture.has_model() {
            self.start_run(RunMode::Continuous);
        }
        tracing::info!(
            adapter,
            output,
            luid_low = selected.adapter_luid.low,
            luid_high = selected.adapter_luid.high,
            "display pipeline committed"
        );
    }

    fn create_capture(&self, adapter: u32, output: u32) -> Result<DxgiCapture> {
        anyhow::ensure!(
            self.graphics.is_some(),
            "graphics COM initialization unavailable"
        );
        GraphicsThread::create_capture(adapter, output)
    }

    /// The state for a new display: a Pipeline when a model is active, otherwise a bare capture.
    fn build_candidate(&self, adapter: u32, output: u32, pipeline_id: u64) -> Result<CaptureState> {
        let capture = self.create_capture(adapter, output)?;
        let Some((_, path)) = self.active_model.as_ref() else {
            return Ok(CaptureState::CaptureOnly(Box::new(capture)));
        };
        let config = PipelineConfig {
            model: path.as_path(),
            pipeline_id,
            confidence_threshold: self.tuning.confidence_threshold,
            low_latency: self.tuning.low_latency,
            qpc: self.qpc,
        };
        match Pipeline::build(capture, &config) {
            Ok(pipeline) => Ok(CaptureState::Ready(Box::new(pipeline))),
            Err((_, error)) => Err(error),
        }
    }

    /// Records the running Pipeline's model in the status. Does nothing without a Pipeline.
    pub(super) fn commit_model_status(&mut self) {
        let Some(pipeline) = self.capture.pipeline() else {
            return;
        };
        self.output_stage.set_input_size(pipeline.input_size());
        super::log_pipeline_state(
            "Running",
            Some(pipeline.id()),
            Some(pipeline.prepare_id()),
            None,
            None,
        );
        let input_size = pipeline.input_size();
        let precision = pipeline.precision();
        let ep_assignment = std::sync::Arc::clone(pipeline.ep_assignment());
        let summary = crate::engine::ep_assignment_summary(&ep_assignment);
        self.status.edit(|status| {
            status.model.phase = ModelPhase::Ready;
            status.model.input_size = Some(input_size);
            status.model.precision = Some(precision);
            status.model.error = None;
            status.model.ep_assignment = Some(ep_assignment);
            status.model.ep_assignment_summary = summary;
        });
    }
}
