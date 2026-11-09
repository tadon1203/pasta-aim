use eframe::egui;

use super::{App, error_label, run_idle};
use nice_aimassist::{
    engine::State,
    hotkeys::{Action, Shortcut},
};

impl App {
    pub(super) fn draw_hotkeys(&mut self, ui: &mut egui::Ui, engine: &State) {
        ui.group(|ui| {
            ui.strong("Hotkeys");
            for action in Action::ALL {
                ui.horizontal_wrapped(|ui| {
                    ui.add_sized([120.0, 20.0], egui::Label::new(action.label()));
                    let shortcuts: Vec<Shortcut> = self
                        .settings
                        .bindings_for(action)
                        .map(|binding| binding.shortcut)
                        .collect();
                    for shortcut in shortcuts {
                        let label = format!("{}  ×", shortcut.label());
                        if ui
                            .add_enabled(run_idle(engine), egui::Button::new(label))
                            .on_hover_text("Remove shortcut")
                            .clicked()
                        {
                            self.remove_hotkey(action, shortcut);
                        }
                    }
                    let can_add = self.hotkey_capture.is_none() && run_idle(engine);
                    if ui
                        .add_enabled(can_add, egui::Button::new("+ Add"))
                        .clicked()
                    {
                        self.begin_hotkey_capture(action);
                    }
                });
            }

            if let Some(error) = &self.hotkey_error {
                error_label(ui, error);
            }
        });
    }

    pub(super) fn draw_hotkey_capture(&mut self, ctx: &egui::Context) {
        let Some(action) = self.hotkey_capture else {
            return;
        };

        let mut open = true;
        egui::Window::new("Add shortcut")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.strong(action.label());
                ui.add_space(8.0);
                ui.heading("Waiting for input…");
                ui.label("Press a key or a controller button. Esc cancels.");
                ui.small("Keyboard shortcuts may use Ctrl, Shift, and Alt. XInput buttons and LT/RT are also supported.");
                if let Some(error) = &self.hotkey_capture_error {
                    ui.add_space(6.0);
                    error_label(ui, error);
                }
                ui.add_space(8.0);
                if ui.button("Cancel").clicked() {
                    self.cancel_hotkey_capture();
                }
            });
        if !open && self.hotkey_capture.is_some() {
            self.cancel_hotkey_capture();
        }
    }
}
