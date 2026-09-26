mod controls;
mod devices;
mod diagnostics;
mod labels;
mod model;
mod widgets;

use eframe::egui;

use self::widgets::{Edit, error_label, warn_label};
use super::App;
use crate::{config::Config, hotkeys::display_shortcut, settings::HotkeyAction};

impl App {
    fn apply_edit(&mut self, edit: Edit, set: impl FnOnce(&mut Self, i32)) {
        if let Some(value) = edit.value {
            set(self, value);
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

        self.process_engine_events();
        let status = self.engine.status();

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("nice-aimassist");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let ready = status.ready_for(self.config.output_mode);
                            let label = if ready {
                                format!("Ready · {}", self.config.output_mode.label())
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
                    self.draw_devices(ui, &status);
                    ui.add_space(6.0);
                    self.draw_model(ui, &status);
                    ui.add_space(6.0);
                    self.draw_output(ui, &status);
                    ui.add_space(6.0);
                    self.draw_calibration(ui, &status);
                    ui.add_space(6.0);
                    self.draw_continuous(ui, &status);
                    ui.add_space(6.0);
                    self.draw_hotkeys(ui, &status);
                    ui.add_space(6.0);
                    self.draw_diagnostics(ui, &status);

                    if let Some(warning) = &self.config_load_warning {
                        ui.add_space(6.0);
                        warn_label(ui, warning);
                    }
                    if let Some(error) = &self.config_save_error {
                        ui.add_space(6.0);
                        error_label(ui, error);
                    }
                });
        });

        self.draw_hotkey_capture(ui.ctx());
    }
}

fn bindings_summary(config: &Config, action: HotkeyAction) -> String {
    config
        .bindings_for(action)
        .map(|binding| display_shortcut(&binding.shortcut))
        .collect::<Vec<_>>()
        .join(", ")
}
