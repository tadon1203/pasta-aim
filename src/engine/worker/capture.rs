//! Display catalog, capture selection, and building a whole capture + inference pipeline.

use super::{Worker, log_pipeline_state, spawn_workers};
use crate::{
    engine::{
        CapturePhase, EngineEvent, ModelPhase, RunMode,
        capture_worker::CaptureWorker,
        inference_worker::{InferenceWorker, PreparedModelInfo},
    },
    graphics::{DisplayInfo, FrameConsumer},
    overlay::Region,
};
use anyhow::Result;

/// A capture + inference pair built for a new display, not yet committed.
struct PreparedPipeline {
    capture: CaptureWorker,
    inference: InferenceWorker,
    consumer: FrameConsumer,
    display: DisplayInfo,
    model: Option<PreparedModelInfo>,
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

        match self
            .capture_worker
            .configure_output(self.capture_adapter, self.capture_output)
        {
            Ok(selected) => {
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
                return;
            }
        };

        // Stop the old workers before they are replaced.
        self.stop_run();
        let PreparedPipeline {
            capture,
            inference,
            consumer,
            display: selected,
            model,
        } = prepared;
        self.capture_adapter = adapter;
        self.capture_output = output;
        self.selected_adapter_luid = selected.adapter_luid;
        self.capture_worker = capture;
        self.inference_worker = inference;
        self.frame_consumer = consumer;
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
        self.refresh_overlay_region();
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

    /// Builds workers for a display (and loads the active model on them) without
    /// touching the running pipeline, so a failure leaves it as it was.
    fn prepare_pipeline(&mut self, adapter: u32, output: u32) -> Result<PreparedPipeline> {
        let pipeline_id = self.next_pipeline_id();
        log_pipeline_state("Preparing", Some(pipeline_id), None, None, None);
        let (capture, inference, consumer) = spawn_workers(
            self.qpc,
            self.tuning,
            &self.mak,
            &self.gate,
            self.overlay_publisher(),
        )?;
        let display = capture.configure_output(adapter, output)?;
        let model = match &self.active_model {
            Some((_, path)) => Some(load_model_into(
                &capture,
                &inference,
                path,
                display.adapter_luid,
                pipeline_id,
            )?),
            None => None,
        };
        Ok(PreparedPipeline {
            capture,
            inference,
            consumer,
            display,
            model,
        })
    }

    /// Points the overlay at the model input region of the committed pipeline,
    /// or at nothing while there is no usable pipeline.
    pub(super) fn refresh_overlay_region(&self) {
        let Some(overlay) = &self.overlay else {
            return;
        };
        let status = self.status.get();
        let region = status
            .capture
            .selected
            .as_ref()
            .zip(status.model.input_size)
            .filter(|_| self.model_ready)
            .and_then(|(display, input)| {
                Region::for_display(
                    display.desktop_origin,
                    (display.width as usize, display.height as usize),
                    input,
                )
            });
        overlay.set_region(region);
    }

    /// Records a freshly loaded model as the running pipeline.
    pub(super) fn commit_model_status(&mut self, model: &PreparedModelInfo) {
        self.active_pipeline_id = Some(model.pipeline_id);
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

/// Loads `path` on `inference` and hands the resulting frame pool to `capture`.
pub(super) fn load_model_into(
    capture: &CaptureWorker,
    inference: &InferenceWorker,
    path: &std::path::Path,
    adapter_luid: crate::inference::AdapterLuid,
    pipeline_id: u64,
) -> Result<PreparedModelInfo> {
    let loaded = inference.load_model(
        path.to_path_buf(),
        adapter_luid,
        capture.transport_device()?,
        capture.publisher(),
        pipeline_id,
    )?;
    capture.configure_frame_pool(loaded.capture_frames.clone())?;
    Ok(loaded)
}
