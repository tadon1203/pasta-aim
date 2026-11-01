use eframe::egui;

use super::widgets::error_label;
use crate::ui::App;
use std::{fs, process::Command};

use anyhow::Context;
use nice_aimassist::{
    engine::{ModelPhase, RunPhase, State},
    pipeline::Precision,
    settings,
};

impl App {
    pub(super) fn draw_model(&mut self, ui: &mut egui::Ui, engine: &State) {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.strong("Model");
                if engine.model.phase == ModelPhase::Loading {
                    ui.spinner();
                    ui.label("Loading…");
                }
            });

            let selected_text = engine
                .model
                .loading_name
                .as_ref()
                .or(engine.model.active_name.as_ref())
                .or(self.settings.model.as_ref())
                .cloned()
                .unwrap_or_else(|| "Select a model".to_owned());

            ui.horizontal(|ui| {
                let can_change =
                    engine.model.phase != ModelPhase::Loading && engine.run.phase == RunPhase::Idle;
                ui.add_enabled_ui(can_change, |ui| {
                    egui::ComboBox::from_id_salt("model_selector")
                        .selected_text(selected_text)
                        .width(330.0)
                        .show_ui(ui, |ui| {
                            let names = self.models.clone();
                            for name in names {
                                let active =
                                    engine.model.active_name.as_deref() == Some(name.as_str());
                                if ui.selectable_label(active, &name).clicked() {
                                    self.select_model(name);
                                }
                            }
                        });
                });

                if ui.button("Refresh").clicked() {
                    self.rescan_models();
                }
                if ui.button("Open models folder").clicked()
                    && let Err(error) = open_models_folder()
                {
                    self.model_catalog_error = Some(format!("{error:#}"));
                }
            });

            if self.models.is_empty() {
                ui.small("No .onnx models found in the models folder.");
            }
            if let Some((width, height)) = engine.model.input_size {
                let precision = engine.model.precision.map_or("", Precision::label);
                ui.small(format!("Active input: {width}×{height} {precision}"));
            }
            if let Some(error) = &engine.model.error {
                error_label(ui, format!("Model: {error}"));
            }
            if let Some(error) = &self.model_catalog_error {
                error_label(ui, format!("Models: {error}"));
            }
        });
    }
}

/// Opens `models/` in Explorer, creating it first.
fn open_models_folder() -> anyhow::Result<()> {
    let dir = settings::models_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Command::new("explorer")
        .arg(&dir)
        .spawn()
        .with_context(|| format!("failed to open {}", dir.display()))?;
    tracing::debug!(path = %dir.display(), "opened models folder");
    Ok(())
}
