use eframe::egui;

use super::{
    labels::{makcu_phase_label, makcu_phase_mark, route_label},
    widgets::error_label,
};
use crate::{
    app::App,
    engine::{Command, EngineState, MakcuPhase, RunPhase},
    graphics::DisplayInfo,
    makcu::format_firmware,
};

impl App {
    pub(super) fn draw_devices(&mut self, ui: &mut egui::Ui, status: &EngineState) {
        ui.group(|ui| {
            ui.strong("Devices");
            ui.horizontal(|ui| {
                ui.label("Capture");
                let selected_text = status
                    .capture
                    .selected
                    .as_ref()
                    .map(DisplayInfo::label)
                    .unwrap_or_else(|| "Select a display".to_owned());
                let can_change = status.run.phase == RunPhase::Idle;
                ui.add_enabled_ui(can_change, |ui| {
                    egui::ComboBox::from_id_salt("capture_display_selector")
                        .selected_text(selected_text)
                        .width(430.0)
                        .show_ui(ui, |ui| {
                            for display in &status.graphics.displays {
                                let active =
                                    status.capture.selected.as_ref().is_some_and(|selected| {
                                        selected.adapter_index == display.adapter_index
                                            && selected.output_index == display.output_index
                                    });
                                if ui.selectable_label(active, display.label()).clicked() {
                                    self.select_display(
                                        display.adapter_index,
                                        display.output_index,
                                    );
                                }
                            }
                        });
                });
                if ui.small_button("Refresh").clicked() {
                    self.rescan_displays();
                }
            });
            if let Some(error) = &status.graphics.catalog_error {
                error_label(ui, format!("Displays: {error}"));
            }
            egui::Grid::new("device_status_grid")
                .num_columns(2)
                .spacing([18.0, 6.0])
                .show(ui, |ui| {
                    ui.label("MAKCU");
                    ui.horizontal(|ui| {
                        ui.label(makcu_phase_mark(status.makcu.phase));
                        ui.label(makcu_phase_label(status.makcu.phase));
                        if let Some(port) = &status.makcu.port {
                            ui.weak(format!("· {port}"));
                        }
                        if let Some(firmware) = status.makcu.firmware {
                            ui.weak(format!("· FW {}", format_firmware(firmware)));
                        }
                        if matches!(
                            status.makcu.phase,
                            MakcuPhase::Connecting | MakcuPhase::Reconnecting
                        ) {
                            ui.spinner();
                        }
                    });
                    ui.end_row();

                    ui.label("MAKCU port");
                    ui.horizontal(|ui| {
                        let chosen = self.config.makcu_port.clone();
                        let selected_text = chosen.clone().unwrap_or_else(|| "Auto".to_owned());
                        let busy = matches!(
                            status.makcu.phase,
                            MakcuPhase::Connecting | MakcuPhase::Reconnecting
                        );
                        ui.add_enabled_ui(!busy, |ui| {
                            egui::ComboBox::from_id_salt("makcu_port_selector")
                                .selected_text(selected_text)
                                .width(200.0)
                                .show_ui(ui, |ui| {
                                    if ui.selectable_label(chosen.is_none(), "Auto").clicked() {
                                        self.set_makcu_port(None);
                                    }
                                    for entry in &status.makcu.ports {
                                        let label = if entry.makcu {
                                            format!("{} · MAKCU", entry.name)
                                        } else {
                                            entry.name.clone()
                                        };
                                        let active = chosen.as_deref() == Some(entry.name.as_str());
                                        if ui.selectable_label(active, label).clicked() {
                                            self.set_makcu_port(Some(entry.name.clone()));
                                        }
                                    }
                                });
                        });
                        if let Some(port) = &chosen
                            && !status.makcu.ports.iter().any(|entry| &entry.name == port)
                        {
                            ui.weak("not found");
                        }
                    });
                    ui.end_row();

                    ui.label("Mouse route");
                    ui.label(route_label(status.makcu.routes.mouse));
                    ui.end_row();

                    ui.label("XInput hotkey pad");
                    ui.label(
                        self.hotkeys
                            .xinput_slot()
                            .map(|slot| format!("● Connected · slot {slot}"))
                            .unwrap_or_else(|| "○ Not connected".to_owned()),
                    );
                    ui.end_row();
                });

            ui.horizontal(|ui| match status.makcu.phase {
                MakcuPhase::Connected => {
                    if ui.button("Refresh routes").clicked() {
                        self.engine.send(Command::RefreshMakcuRoutes);
                    }
                    if ui.button("Rescan device").clicked() {
                        self.engine.send(Command::RescanMakcu);
                    }
                }
                MakcuPhase::Connecting | MakcuPhase::Reconnecting => {
                    ui.add_enabled(false, egui::Button::new("Connecting…"));
                }
                MakcuPhase::Failed => {
                    if ui.button("Connect MAKCU").clicked() {
                        self.engine.send(Command::RescanMakcu);
                    }
                }
            });

            if let Some(error) = &status.makcu.error {
                error_label(ui, format!("MAKCU: {error}"));
            }
        });
    }
}
