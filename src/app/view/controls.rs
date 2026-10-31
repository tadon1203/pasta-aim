use eframe::egui;

use super::{
    bindings_summary,
    widgets::{drag_value, error_label, percent_row, warn_label},
};
use crate::{
    app::{App, EditField},
    engine::{CapturePhase, Command, EngineState, ModelPhase, RunPhase},
    hotkeys::{CaptureResult, capture_from_events, display_shortcut},
    settings::{
        CaptureSettings, ConfidenceThreshold, FpsLimit, HotkeyAction, Interpolation,
        InterpolationPercent, MaxDelta, Sensitivity, TargetYOffset,
    },
    telemetry::LiveTelemetry,
};

/// The fixed interpolation that a switch from Auto starts with.
const FIXED_INTERPOLATION_START: u8 = 50;

impl App {
    pub(super) fn draw_output(&mut self, ui: &mut egui::Ui, status: &EngineState) {
        ui.group(|ui| {
            ui.strong("Output");
            if !status.makcu.routes.mouse {
                warn_label(ui, "Waiting for a mouse route.");
            }

            ui.horizontal(|ui| {
                ui.label("Move interpolation");
                let mut auto = self.config.interpolation == Interpolation::Auto;
                if ui.checkbox(&mut auto, "Auto").changed() {
                    self.set_interpolation(if auto {
                        Interpolation::Auto
                    } else {
                        Interpolation::Percent(InterpolationPercent::new(FIXED_INTERPOLATION_START))
                    });
                    self.persist();
                }
            });
            if let Interpolation::Percent(percent) = self.config.interpolation {
                let edit = percent_row(
                    ui,
                    "Fixed",
                    percent.value(),
                    InterpolationPercent::MIN..=InterpolationPercent::MAX,
                    self.dirty.contains(&EditField::Interpolation),
                );
                self.apply_edit(EditField::Interpolation, edit, |app, value| {
                    app.set_interpolation(Interpolation::Percent(InterpolationPercent::new(value)));
                });
            }
        });
    }

    pub(super) fn draw_calibration(
        &mut self,
        ui: &mut egui::Ui,
        status: &EngineState,
        live: &LiveTelemetry,
    ) {
        ui.group(|ui| {
            ui.strong("Calibration");
            let edit = percent_row(
                ui,
                "Sensitivity",
                self.config.sensitivity.value(),
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
                self.config.confidence_threshold.value(),
                ConfidenceThreshold::MIN..=ConfidenceThreshold::MAX,
                self.dirty.contains(&EditField::ConfidenceThreshold),
            );
            self.apply_edit(EditField::ConfidenceThreshold, edit, |app, value| {
                app.set_confidence_threshold(ConfidenceThreshold::new(value));
            });
            ui.small("Minimum confidence required to accept a detection.");

            let edit = percent_row(
                ui,
                "Target Y offset",
                self.config.target_y_offset.value(),
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
                    self.config.max_delta.value(),
                    0..=MaxDelta::MAX,
                    self.dirty.contains(&EditField::MaxDelta),
                );
                self.apply_edit(EditField::MaxDelta, edit, |app, value| {
                    app.set_max_delta(MaxDelta::new(value));
                });
            });
            ui.small("Per-frame cap on movement length after sensitivity; 0 = unlimited.");

            let ready = status.ready();
            let run_idle = status.run.phase == RunPhase::Idle;
            if ui
                .add_enabled(ready && run_idle, egui::Button::new("Run once"))
                .clicked()
            {
                self.engine.send(Command::RunOnce);
            }

            let once_shortcuts = bindings_summary(&self.config, HotkeyAction::RunOnce);
            if !once_shortcuts.is_empty() {
                ui.small(format!("Shortcuts: {once_shortcuts}"));
            }

            if !ready && let Some(reason) = status.block_reason() {
                ui.horizontal(|ui| {
                    ui.small(reason);
                    if status.capture.phase == CapturePhase::Failed
                        && ui.small_button("Retry capture").clicked()
                    {
                        self.engine.send(Command::RetryCapture);
                    }
                });
            }

            if live.last_target.is_some()
                || live.last_movement.is_some()
                || live.last_total_ms.is_some()
            {
                ui.separator();
                ui.small("Last run");
                if let Some((dx, dy, _, _)) = live.last_target {
                    ui.label(format!("Target     x={dx:+.1} px   y={dy:+.1} px"));
                } else {
                    ui.label("Target     no detection");
                }
                if let Some(sent) = live.last_movement {
                    ui.label(format!("Command    x={:+}   y={:+}", sent.x, sent.y));
                }
                if let Some(ms) = live.last_total_ms {
                    ui.label(format!("Pipeline   {ms:.2} ms"));
                }
            }

            if let Some(error) = &status.run.error {
                error_label(ui, error);
            }
        });
    }

    pub(super) fn draw_continuous(
        &mut self,
        ui: &mut egui::Ui,
        status: &EngineState,
        live: &LiveTelemetry,
    ) {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.strong("Continuous");
                ui.separator();
                ui.label(match status.run.phase {
                    RunPhase::Continuous => "● Running",
                    RunPhase::OneShot => "Single run",
                    RunPhase::Idle => "Idle",
                });
            });
            ui.horizontal(|ui| {
                ui.label("FPS limit");
                let edit = drag_value(
                    ui,
                    self.config.fps_limit.value(),
                    FpsLimit::MIN..=FpsLimit::MAX,
                    self.dirty.contains(&EditField::FpsLimit),
                );
                self.apply_edit(EditField::FpsLimit, edit, |app, value| {
                    app.set_fps_limit(FpsLimit::new(value));
                });
                ui.small("Continuous only");
            });

            let mut capture = self.config.capture;
            ui.horizontal(|ui| {
                ui.label("New frame wait");
                ui.add(egui::DragValue::new(&mut capture.wait_ms).range(CaptureSettings::WAIT_RANGE).suffix(" ms"))
                    .on_hover_text("Maximum wait per capture attempt. FPS limit still applies to new frames.");
            });
            if capture != self.config.capture {
                self.config.capture = capture;
                self.engine.send(Command::SetCaptureSettings(capture));
                self.persist();
            }

            let can_change_latency =
                status.run.phase == RunPhase::Idle && status.model.phase != ModelPhase::Loading;
            ui.add_enabled_ui(can_change_latency, |ui| {
                let mut low_latency = self.config.low_latency;
                if ui
                    .checkbox(&mut low_latency, "Low-latency mode")
                    .on_hover_text(
                        "Busy-waits for the GPU instead of sleeping. Lowers worst-case latency                          but keeps one CPU core fully busy while running. Reloads the model.",
                    )
                    .changed()
                {
                    self.config.low_latency = low_latency;
                    self.engine.send(Command::SetLowLatency(low_latency));
                    self.persist();
                }
            });

            let hold_shortcuts = bindings_summary(&self.config, HotkeyAction::ContinuousHold);
            if hold_shortcuts.is_empty() {
                ui.small("No hold shortcut assigned.");
            } else {
                ui.small(format!("Hold: {hold_shortcuts}"));
            }
            if live.performance.sample_count > 0 {
                if self.gpu_debug_at_startup {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "GPU debug is on: timings are inflated.",
                    );
                }
                ui.small(format!(
                    "{:.1} FPS / {} cap · pipeline {:.2}/{:.2} ms p50/p95 · inference {:.2}/{:.2} ms p50/p95",
                    live.performance.fps,
                    self.config.fps_limit.value(),
                    live.performance.total.p50,
                    live.performance.total.p95,
                    live.performance.inference.p50,
                    live.performance.inference.p95,
                ));
            }
        });
    }

    pub(super) fn draw_hotkeys(&mut self, ui: &mut egui::Ui, status: &EngineState) {
        ui.group(|ui| {
            ui.strong("Hotkeys");
            for action in HotkeyAction::ALL {
                ui.horizontal_wrapped(|ui| {
                    ui.add_sized([120.0, 20.0], egui::Label::new(action.label()));
                    let shortcuts: Vec<String> = self
                        .config
                        .bindings_for(action)
                        .map(|binding| binding.shortcut.clone())
                        .collect();
                    for shortcut in shortcuts {
                        let label = format!("{}  ×", display_shortcut(&shortcut));
                        if ui
                            .add_enabled(
                                status.run.phase == RunPhase::Idle,
                                egui::Button::new(label),
                            )
                            .on_hover_text("Remove shortcut")
                            .clicked()
                        {
                            self.remove_hotkey(action, &shortcut);
                        }
                    }
                    let can_add =
                        self.hotkey_capture.is_none() && status.run.phase == RunPhase::Idle;
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
    pub(super) fn draw_hotkey_capture(&mut self, ctx: &egui::Context) {
        let Some(capture) = self.hotkey_capture.as_mut() else {
            return;
        };
        let action = capture.action;

        if !capture.armed {
            // Do not capture the Enter/Space key that may have activated `+ Add`
            // with keyboard navigation. Capture begins on the next egui pass.
            capture.armed = true;
            ctx.request_repaint();
        } else if let Some(shortcut) = self.hotkeys.poll_captured_xinput() {
            self.add_hotkey(action, &shortcut);
        } else {
            let super_down = ctx.input(|input| {
                input.key_down(egui::Key::SuperLeft) || input.key_down(egui::Key::SuperRight)
            });
            if super_down {
                self.hotkey_capture_error =
                    Some("Windows/Super shortcuts are not supported by the capture UI".to_owned());
            } else {
                let events = ctx.input(|input| input.events.clone());
                if let Some(result) = capture_from_events(&events) {
                    match result {
                        Ok(CaptureResult::Cancelled) => self.cancel_hotkey_capture(),
                        Ok(CaptureResult::Captured(hotkey)) => {
                            self.add_hotkey(action, &hotkey.to_string());
                        }
                        Err(error) => self.hotkey_capture_error = Some(error),
                    }
                }
            }
        }

        if self.hotkey_capture.is_none() {
            return;
        }

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
                ui.label("Press a key, controller button, or trigger.");
                ui.small("Esc cancels. Keyboard shortcuts may use Ctrl, Shift, and Alt. XInput buttons and LT/RT are also supported.");
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
