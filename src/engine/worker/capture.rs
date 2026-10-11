//! Display catalog, capture selection, and building a whole capture + inference pipeline.

use super::{Tuning, Worker, log_pipeline_state};
use crate::{
    engine::{CapturePhase, EngineEvent, ModelPhase, RunMode},
    graphics::{DisplayInfo, DxgiCapture, shared_frame::CaptureFrame},
    inference::{
        EpAssignmentState, TensorPrecision,
        pipeline::{InferencePipeline, prepare_directml_pipeline},
    },
};
use anyhow::Result;
use std::sync::Arc;

/// GPU resources precede the capture owner in the engine's drop order.
pub(super) struct PreparedModel {
    pub pipeline: InferencePipeline,
    pub capture_frame: CaptureFrame,
    pub input_size: (usize, usize),
    pub pipeline_id: u64,
    pub prepare_id: String,
    pub precision: TensorPrecision,
    pub ep_assignment: Arc<EpAssignmentState>,
    pub last_completion: u64,
}

struct PreparedPipeline {
    model: Option<PreparedModel>,
    capture: DxgiCapture,
    display: DisplayInfo,
}

impl Worker {
    pub(super) fn refresh_capture_catalog(&mut self) {
        let Some(graphics) = self.graphics.as_ref() else {
            return;
        };
        match graphics.enumerate_displays() {
            Ok(displays) => self.status.edit(|status| {
                status.graphics.displays = displays;
                status.graphics.catalog_error = None;
            }),
            Err(error) => {
                let error = format!("{error:#}");
                tracing::error!(%error, "graphics enumeration failed");
                self.status.edit(|status| {
                    status.graphics.displays.clear();
                    status.graphics.catalog_error = Some(error);
                });
            }
        }
    }

    /// The catalog entry for the remembered adapter and output, if it is present.
    pub(super) fn selected_display(&self) -> Option<DisplayInfo> {
        self.status
            .get()
            .graphics
            .displays
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
                self.capture = Some(capture);
                self.selected_adapter_luid = selected.adapter_luid;
                self.status.edit(|status| {
                    status.capture.phase = CapturePhase::Ready;
                    status.capture.selected = Some(selected);
                    status.capture.error = None;
                });
                tracing::info!("selected capture initialized");
            }
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "selected capture initialization failed");
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
        let current = self.status.get().capture.selected.clone();
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
        let prepared = match self.prepare_pipeline(adapter, output) {
            Ok(prepared) => prepared,
            Err(error) => {
                let message = format!("Display change failed: {error:#}");
                self.status.edit(|status| {
                    status.capture.phase = if status.capture.selected.is_some() {
                        CapturePhase::Ready
                    } else {
                        CapturePhase::Failed
                    };
                    status.capture.error = Some(message.clone());
                });
                self.emit_event(EngineEvent::Capture {
                    adapter,
                    output,
                    adapter_luid: current
                        .map(|display| display.adapter_luid)
                        .unwrap_or(self.selected_adapter_luid),
                    result: Err(message.clone()),
                });
                tracing::error!(adapter, output, %message, "display prepare failed; active pipeline retained");
                if resume_continuous && self.model_ready {
                    self.start_run(RunMode::Continuous);
                }
                return;
            }
        };

        // Replace only after all candidate resources have been prepared.
        let PreparedPipeline {
            capture,
            display: selected,
            model,
        } = prepared;
        self.capture_adapter = adapter;
        self.capture_output = output;
        self.selected_adapter_luid = selected.adapter_luid;
        self.metrics.mark_cold_next();
        self.model_ready = model.is_some();
        self.active_pipeline_id = None;
        if let Some(model) = &model {
            self.commit_model_status(model);
        }
        self.status.edit(|status| {
            status.capture.phase = CapturePhase::Ready;
            status.capture.selected = Some(selected.clone());
            status.capture.desktop_size = None;
            status.capture.error = None;
            status.run.error = None;
        });
        self.model = model;
        self.capture = Some(capture);
        self.emit_event(EngineEvent::Capture {
            adapter,
            output,
            adapter_luid: selected.adapter_luid,
            result: Ok(()),
        });
        if resume_continuous && self.model_ready {
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
        self.graphics
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("graphics COM initialization unavailable"))?
            .create_capture(adapter, output)
    }

    /// Prepare synchronously, retaining the old resources until full success.
    fn prepare_pipeline(&mut self, adapter: u32, output: u32) -> Result<PreparedPipeline> {
        let pipeline_id = self.next_pipeline_id();
        log_pipeline_state("Preparing", Some(pipeline_id), None, None, None);
        let capture = self.create_capture(adapter, output)?;
        let display = capture.info().clone();
        let model = self
            .active_model
            .as_ref()
            .map(|(_, path)| prepare_model(&capture, path, pipeline_id, self.tuning))
            .transpose()?;
        Ok(PreparedPipeline {
            model,
            capture,
            display,
        })
    }

    /// Records a freshly loaded model as the running pipeline.
    pub(super) fn commit_model_status(&mut self, model: &PreparedModel) {
        self.active_pipeline_id = Some(model.pipeline_id);
        self.output_stage.set_input_size(model.input_size);
        log_pipeline_state(
            "Running",
            Some(model.pipeline_id),
            Some(&model.prepare_id),
            None,
            None,
        );
        let summary = crate::engine::ep_assignment_summary(&model.ep_assignment);
        self.status.edit(|status| {
            status.model.phase = ModelPhase::Ready;
            status.model.input_size = Some(model.input_size);
            status.model.precision = Some(model.precision);
            status.model.error = None;
            status.model.ep_assignment = Some(std::sync::Arc::clone(&model.ep_assignment));
            status.model.ep_assignment_summary = summary;
        });
    }
}

/// Build a replacement model without altering the currently committed resources.
pub(super) fn prepare_model(
    capture: &DxgiCapture,
    path: &std::path::Path,
    pipeline_id: u64,
    tuning: Tuning,
) -> Result<PreparedModel> {
    let prepared = prepare_directml_pipeline(
        path,
        capture.info().adapter_luid,
        &capture.transport_device(),
        pipeline_id,
        tuning.confidence_threshold,
        tuning.low_latency,
    )?;
    let input_size = (prepared.input_spec.width, prepared.input_spec.height);
    if input_size.0 > capture.info().width as usize || input_size.1 > capture.info().height as usize
    {
        anyhow::bail!("model input exceeds the selected display dimensions");
    }
    Ok(PreparedModel {
        pipeline: prepared.pipeline,
        capture_frame: prepared.capture_frames,
        input_size,
        pipeline_id,
        prepare_id: prepared.prepare_id,
        precision: prepared.input_spec.precision,
        ep_assignment: Arc::new(prepared.ep_assignment),
        last_completion: 0,
    })
}
