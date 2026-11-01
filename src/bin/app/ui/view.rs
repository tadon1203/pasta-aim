mod controls;
mod devices;
mod diagnostics;
mod labels;
mod model;
mod widgets;

use std::sync::Arc;

use eframe::egui;

use self::widgets::{Edit, error_label, warn_label};
use super::App;
use crate::ui::EditField;
use nice_aimassist::{hotkeys::Action, settings::Settings};

impl App {
    fn apply_edit<T>(&mut self, field: EditField, edit: Edit<T>, set: impl FnOnce(&mut Self, T)) {
        if let Some(value) = edit.value {
            set(self, value);
        }
        if edit.dirty {
            self.dirty.insert(field);
        } else {
            self.dirty.remove(&field);
        }
        if edit.persist {
            self.persist();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let regained_focus = ui.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::WindowFocused(true)))
        });
        if regained_focus {
            self.rescan_models();
            self.rescan_displays();
        }

        self.process_messages();
        let engine = Arc::clone(&self.state);
        let stats = self.stats;

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("nice-aimassist");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let ready = engine.blocked().is_none();
                            let label = if ready {
                                "Ready".to_owned()
                            } else {
                                "Not ready".to_owned()
                            };
                            if ready {
                                ui.label(label);
                            } else {
                                warn_label(ui, label);
                            }
                        });
                    });

                    ui.add_space(8.0);
                    self.draw_devices(ui, &engine);
                    ui.add_space(6.0);
                    self.draw_model(ui, &engine);
                    ui.add_space(6.0);
                    self.draw_output(ui, &engine);
                    ui.add_space(6.0);
                    self.draw_calibration(ui, &engine, &stats);
                    ui.add_space(6.0);
                    self.draw_continuous(ui, &engine, &stats);
                    ui.add_space(6.0);
                    self.draw_hotkeys(ui, &engine);
                    ui.add_space(6.0);
                    self.draw_diagnostics(ui, &engine, &stats);

                    if let Some(warning) = &self.settings_warning {
                        ui.add_space(6.0);
                        warn_label(ui, warning);
                    }
                    if let Some(error) = &self.settings_save_error {
                        ui.add_space(6.0);
                        error_label(ui, error);
                    }
                });
        });

        self.draw_hotkey_capture(ui.ctx());
    }
}

fn bindings_summary(settings: &Settings, action: Action) -> String {
    settings
        .bindings_for(action)
        .map(|binding| binding.shortcut.label())
        .collect::<Vec<_>>()
        .join(", ")
}
