use eframe::egui;

use super::{App, controller_line, error_label, makcu_line, run_idle};
use nice_aimassist::{
    engine::{Command, OutputPhase, State},
    pipeline::DisplayInfo,
};

fn is_busy(phase: OutputPhase) -> bool {
    phase == OutputPhase::Connecting
}

fn route_label(available: bool) -> &'static str {
    if available {
        "● Ready"
    } else {
        "— Not routed"
    }
}

impl App {
    pub(super) fn draw_devices(&mut self, ui: &mut egui::Ui, engine: &State) {
        ui.group(|ui| {
            ui.strong("Devices");
            self.draw_capture_selector(ui, engine);
            if let Some(error) = &engine.graphics.catalog_error {
                error_label(ui, format!("Displays: {error}"));
            }
            self.draw_makcu_status(ui, engine);
            self.draw_makcu_actions(ui, engine);
            if let Some(error) = &engine.output.error {
                error_label(ui, format!("MAKCU: {error}"));
            }
        });
    }

    fn draw_capture_selector(&mut self, ui: &mut egui::Ui, engine: &State) {
        ui.horizontal(|ui| {
            ui.label("Capture");
            let selected_text = engine
                .capture
                .selected
                .as_ref()
                .map_or_else(|| "Select a display".to_owned(), DisplayInfo::label);
            ui.add_enabled_ui(run_idle(engine), |ui| {
                egui::ComboBox::from_id_salt("capture_display_selector")
                    .selected_text(selected_text)
                    .width(430.0)
                    .show_ui(ui, |ui| {
                        for display in &engine.graphics.displays {
                            let active = engine.capture.selected.as_ref().is_some_and(|selected| {
                                selected.adapter_index == display.adapter_index
                                    && selected.output_index == display.output_index
                            });
                            if ui.selectable_label(active, display.label()).clicked() {
                                self.select_display(display.selection());
                            }
                        }
                    });
            });
            if ui.small_button("Refresh").clicked() {
                self.rescan_displays();
            }
        });
    }

    fn draw_makcu_status(&mut self, ui: &mut egui::Ui, engine: &State) {
        egui::Grid::new("device_status_grid")
            .num_columns(2)
            .spacing([18.0, 6.0])
            .show(ui, |ui| {
                ui.label("MAKCU");
                ui.horizontal(|ui| {
                    ui.label(makcu_line(
                        engine.output.phase,
                        engine.output.device.as_ref(),
                    ));
                    if is_busy(engine.output.phase) {
                        ui.spinner();
                    }
                });
                ui.end_row();

                self.draw_makcu_port_row(ui, engine);

                ui.label("Mouse route");
                let mouse_route = engine
                    .output
                    .device
                    .as_ref()
                    .is_some_and(|device| device.mouse_route);
                ui.label(route_label(mouse_route));
                ui.end_row();

                ui.label("XInput hotkey pad");
                ui.label(controller_line(self.controller_slot));
                ui.end_row();
            });
    }

    fn draw_makcu_port_row(&mut self, ui: &mut egui::Ui, engine: &State) {
        ui.label("MAKCU port");
        ui.horizontal(|ui| {
            let chosen = self.settings.makcu_port.clone();
            let selected_text = chosen.clone().unwrap_or_else(|| "Auto".to_owned());
            ui.add_enabled_ui(!is_busy(engine.output.phase), |ui| {
                egui::ComboBox::from_id_salt("makcu_port_selector")
                    .selected_text(selected_text)
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(chosen.is_none(), "Auto").clicked() {
                            self.edit(|settings| settings.makcu_port = None);
                            self.persist();
                        }
                        for entry in &engine.output.ports {
                            let label = if entry.description.is_empty() {
                                entry.name.clone()
                            } else {
                                format!("{} · {}", entry.name, entry.description)
                            };
                            let active = chosen.as_deref() == Some(entry.name.as_str());
                            if ui.selectable_label(active, label).clicked() {
                                let port = entry.name.clone();
                                self.edit(|settings| settings.makcu_port = Some(port));
                                self.persist();
                            }
                        }
                    });
            });
            if let Some(port) = &chosen
                && !engine.output.ports.iter().any(|entry| &entry.name == port)
            {
                ui.weak("not found");
            }
        });
        ui.end_row();
    }

    fn draw_makcu_actions(&mut self, ui: &mut egui::Ui, engine: &State) {
        ui.horizontal(|ui| {
            if engine.output.phase == OutputPhase::Connecting {
                ui.add_enabled(false, egui::Button::new("Connecting…"));
            } else if ui.button("Reconnect").clicked() {
                self.commands.send(Command::ReconnectOutput);
            }
        });
    }
}
