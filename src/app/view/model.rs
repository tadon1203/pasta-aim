use eframe::egui;

use super::widgets::error_label;
use crate::{
    app::App,
    engine::{EngineState, ModelPhase, RunPhase},
    inference::TensorPrecision,
    models,
};

impl App {
    pub(super) fn draw_model(&mut self, ui: &mut egui::Ui, status: &EngineState) {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.strong("Model");
                if status.model.phase == ModelPhase::Loading {
                    ui.spinner();
                    ui.label("Loading…");
                }
            });

            let selected_text = status
                .model
                .loading_name
                .as_ref()
                .or(status.model.active_name.as_ref())
                .or(self.config.model.as_ref())
                .cloned()
                .unwrap_or_else(|| "Select a model".to_owned());

            ui.horizontal(|ui| {
                let can_change =
                    status.model.phase != ModelPhase::Loading && status.run.phase == RunPhase::Idle;
                ui.add_enabled_ui(can_change, |ui| {
                    egui::ComboBox::from_id_salt("model_selector")
                        .selected_text(selected_text)
                        .width(330.0)
                        .show_ui(ui, |ui| {
                            let entries = self.models.entries().to_vec();
                            for entry in entries {
                                let active = status.model.active_name.as_deref()
                                    == Some(entry.name.as_str());
                                if ui.selectable_label(active, &entry.name).clicked() {
                                    self.select_model(entry.name);
                                }
                            }
                        });
                });

                if ui.button("Refresh").clicked() {
                    self.rescan_models();
                }
                if ui.button("Open models folder").clicked()
                    && let Err(error) = models::open_models_folder()
                {
                    self.model_catalog_error = Some(format!("{error:#}"));
                }
            });

            if self.models.is_empty() {
                ui.small("No .onnx models found in the models folder.");
            }
            if let Some((width, height)) = status.model.input_size {
                let precision = status.model.precision.map_or("", TensorPrecision::label);
                ui.small(format!("Active input: {width}×{height} {precision}"));
            }
            if let Some(error) = &status.model.error {
                error_label(ui, format!("Model: {error}"));
            }
            if let Some(error) = &self.model_catalog_error {
                error_label(ui, format!("Models: {error}"));
            }
        });
    }
}
