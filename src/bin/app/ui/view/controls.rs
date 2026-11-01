use eframe::egui;

use super::{
    bindings_summary,
    widgets::{drag_value, error_label, percent_row, warn_label},
};
use crate::ui::{App, EditField};
use nice_aimassist::{
    engine::{CapturePhase, Command, ModelPhase, RunPhase, State, Stats},
    hotkeys::{Action, Shortcut},
    settings::{
        Confidence, FpsLimit, Interpolation, InterpolationPercent, MaxDelta, NewFrameWait,
        Sensitivity, TargetYOffset,
    },
};

/// The fixed interpolation that a switch from Auto starts with.
const FIXED_INTERPOLATION_START: u16 = 50;

impl App {
    pub(super) fn draw_output(&mut self, ui: &mut egui::Ui, engine: &State) {
        ui.group(|ui| {
            ui.strong("Output");
            if !engine
                .output
                .device
                .as_ref()
                .is_some_and(|device| device.mouse_route)
            {
                warn_label(ui, "Waiting for a mouse route.");
            }

            ui.horizontal(|ui| {
                ui.label("Move interpolation");
                let mut auto = self.settings.interpolation == Interpolation::Auto;
                if ui.checkbox(&mut auto, "Auto").changed() {
                    self.set_interpolation(if auto {
                        Interpolation::Auto
                    } else {
                        Interpolation::Percent(InterpolationPercent::new(FIXED_INTERPOLATION_START))
                    });
                    self.persist();
                }
            });
            if let Interpolation::Percent(percent) = self.settings.interpolation {
                let edit = percent_row(
                    ui,
                    "Fixed",
                    percent.get(),
                    InterpolationPercent::MIN..=InterpolationPercent::MAX,
                    self.dirty.contains(&EditField::Interpolation),
                );
                self.apply_edit(EditField::Interpolation, edit, |app, value| {
                    app.set_interpolation(Interpolation::Percent(InterpolationPercent::new(value)));
                });
            }
        });
    }

    pub(super) fn draw_calibration(&mut self, ui: &mut egui::Ui, engine: &State, stats: &Stats) {
        ui.group(|ui| {
            ui.strong("Calibration");
            let edit = percent_row(
                ui,
                "Sensitivity",
                self.settings.sensitivity.get(),
                Sensitivity::MIN..=Sensitivity::MAX,
                self.dirty.contains(&EditField::Sensitivity),
            );
            self.apply_edit(EditField::Sensitivity, edit, |app, value| {
                app.set_sensitivity(Sensitivity::new(value));
            });
            ui.small("100% = 1× command scale");

            let edit = percent_row(
                ui,
                "Confidence",
                self.settings.confidence.get(),
                Confidence::MIN..=Confidence::MAX,
                self.dirty.contains(&EditField::Confidence),
            );
            self.apply_edit(EditField::Confidence, edit, |app, value| {
                app.set_confidence(Confidence::new(value));
            });
            ui.small("Minimum confidence required to accept a detection.");

            let edit = percent_row(
                ui,
                "Target Y offset",
                self.settings.target_y_offset.get(),
                TargetYOffset::MIN..=TargetYOffset::MAX,
                self.dirty.contains(&EditField::TargetYOffset),
            );
            self.apply_edit(EditField::TargetYOffset, edit, |app, value| {
                app.set_target_y_offset(TargetYOffset::new(value));
            });
            ui.small("Aim point inside the target box: 0% = top, 50% = center, 100% = bottom.");

            ui.horizontal(|ui| {
                ui.label("Max delta");
                let edit = drag_value(
                    ui,
                    self.settings.max_delta.get(),
                    0..=MaxDelta::MAX,
                    self.dirty.contains(&EditField::MaxDelta),
                );
                self.apply_edit(EditField::MaxDelta, edit, |app, value| {
                    app.set_max_delta(MaxDelta::new(value));
                });
            });
            ui.small("Per-frame cap on movement length after sensitivity; 0 = unlimited.");

            let ready = engine.ready();
            let run_idle = engine.run.phase == RunPhase::Idle;
            if ui
                .add_enabled(ready && run_idle, egui::Button::new("Run once"))
                .clicked()
            {
                self.commands.send(Command::RunOnce);
            }

            let once_shortcuts = bindings_summary(&self.settings, Action::RunOnce);
            if !once_shortcuts.is_empty() {
                ui.small(format!("Shortcuts: {once_shortcuts}"));
            }

            if !ready && let Some(reason) = engine.block_reason() {
                ui.horizontal(|ui| {
                    ui.small(reason);
                    if engine.capture.phase == CapturePhase::Failed
                        && ui.small_button("Retry capture").clicked()
                    {
                        self.commands.send(Command::RetryCapture);
                    }
                });
            }

            if stats.last_target.is_some()
                || stats.last_move.is_some()
                || stats.last_total_ms.is_some()
            {
                ui.separator();
                ui.small("Last run");
                if let Some((dx, dy, _, _)) = stats.last_target {
                    ui.label(format!("Target     x={dx:+.1} px   y={dy:+.1} px"));
                } else {
                    ui.label("Target     no detection");
                }
                if let Some(sent) = stats.last_move {
                    ui.label(format!("Command    x={:+}   y={:+}", sent.x, sent.y));
                }
                if let Some(ms) = stats.last_total_ms {
                    ui.label(format!("pipeline   {ms:.2} ms"));
                }
            }

            if let Some(error) = &engine.run.error {
                error_label(ui, error);
            }
        });
    }

    pub(super) fn draw_continuous(&mut self, ui: &mut egui::Ui, engine: &State, stats: &Stats) {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.strong("Continuous");
                ui.separator();
                ui.label(match engine.run.phase {
                    RunPhase::Continuous => "● Running",
                    RunPhase::OneShot => "Single run",
                    RunPhase::Idle => "Idle",
                });
            });
            ui.horizontal(|ui| {
                ui.label("FPS limit");
                let edit = drag_value(
                    ui,
                    self.settings.fps_limit.get(),
                    FpsLimit::MIN..=FpsLimit::MAX,
                    self.dirty.contains(&EditField::FpsLimit),
                );
                self.apply_edit(EditField::FpsLimit, edit, |app, value| {
                    app.set_fps_limit(FpsLimit::new(value));
                });
                ui.small("Continuous only");
            });

            let mut wait = self.settings.new_frame_wait;
            ui.horizontal(|ui| {
                ui.label("New frame wait");
                let mut milliseconds = wait.get();
                ui.add(egui::DragValue::new(&mut milliseconds).range(NewFrameWait::MIN..=NewFrameWait::MAX).suffix(" ms"))
                    .on_hover_text("Maximum wait per capture attempt. FPS limit still applies to new frames.");
                wait = NewFrameWait::new(milliseconds);
            });
            if wait != self.settings.new_frame_wait {
                self.settings.new_frame_wait = wait;
                self.commands.send(Command::SetNewFrameWait(wait));
                self.persist();
            }

            let can_change_latency =
                engine.run.phase == RunPhase::Idle && engine.model.phase != ModelPhase::Loading;
            ui.add_enabled_ui(can_change_latency, |ui| {
                let mut low_latency = self.settings.low_latency;
                if ui
                    .checkbox(&mut low_latency, "Low-latency mode")
                    .on_hover_text(
                        "Busy-waits for the GPU instead of sleeping. Lowers worst-case latency                          but keeps one CPU core fully busy while running. Reloads the model.",
                    )
                    .changed()
                {
                    self.settings.low_latency = low_latency;
                    self.commands.send(Command::SetLowLatency(low_latency));
                    self.persist();
                }
            });

            let hold_shortcuts = bindings_summary(&self.settings, Action::Hold);
            if hold_shortcuts.is_empty() {
                ui.small("No hold shortcut assigned.");
            } else {
                ui.small(format!("Hold: {hold_shortcuts}"));
            }
            if stats.stages.sample_count > 0 {
                if self.gpu_debug_at_startup {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "GPU debug is on: timings are inflated.",
                    );
                }
                ui.small(format!(
                    "{:.1} FPS / {} cap · pipeline {:.2}/{:.2} ms p50/p95 · inference {:.2}/{:.2} ms p50/p95",
                    stats.stages.fps,
                    self.settings.fps_limit.get(),
                    stats.stages.total.p50,
                    stats.stages.total.p95,
                    stats.stages.inference.p50,
                    stats.stages.inference.p95,
                ));
            }
        });
    }

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
                            .add_enabled(
                                engine.run.phase == RunPhase::Idle,
                                egui::Button::new(label),
                            )
                            .on_hover_text("Remove shortcut")
                            .clicked()
                        {
                            self.remove_hotkey(action, shortcut);
                        }
                    }
                    let can_add =
                        self.hotkey_capture.is_none() && engine.run.phase == RunPhase::Idle;
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
}

impl App {
    /// The dialog. The hotkeys thread records the shortcut; this only shows the wait and the result.
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
