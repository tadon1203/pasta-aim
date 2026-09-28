use eframe::egui;

use super::{
    labels::{mak_phase_label, mak_phase_mark, route_label},
    widgets::error_label,
};
use crate::{
    app::App,
    engine::{Command, EngineStatus, MakPhase, RunPhase},
    graphics::DisplayInfo,
};

impl App {
    pub(super) fn draw_devices(&mut self, ui: &mut egui::Ui, status: &EngineStatus) {
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
                    ui.label("MAK");
                    ui.horizontal(|ui| {
                        ui.label(mak_phase_mark(status.mak.phase));
                        ui.label(mak_phase_label(status.mak.phase));
                        if let Some(port) = &status.mak.port {
                            ui.weak(format!("· {port}"));
                        }
                        if let Some(firmware) = status.mak.firmware {
                            ui.weak(format!("· FW {firmware}"));
                        }
                        if matches!(
                            status.mak.phase,
                            MakPhase::Connecting | MakPhase::Reconnecting
                        ) {
                            ui.spinner();
                        }
                    });
                    ui.end_row();

                    ui.label("Mouse route");
                    ui.label(route_label(status.mak.routes.mouse));
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

            ui.horizontal(|ui| match status.mak.phase {
                MakPhase::Connected => {
                    if ui.button("Refresh routes").clicked() {
                        self.engine.send(Command::RefreshMakRoutes);
                    }
                    if ui.button("Rescan device").clicked() {
                        self.engine.send(Command::RescanMak);
                    }
                }
                MakPhase::Connecting | MakPhase::Reconnecting => {
                    ui.add_enabled(false, egui::Button::new("Connecting…"));
                }
                MakPhase::Failed => {
                    if ui.button("Connect MAK").clicked() {
                        self.engine.send(Command::RescanMak);
                    }
                }
            });

            if let Some(error) = &status.mak.error {
                error_label(ui, format!("MAK: {error}"));
            }
        });
    }
}
