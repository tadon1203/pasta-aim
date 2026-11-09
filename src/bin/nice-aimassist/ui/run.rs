use std::ops::RangeInclusive;

use eframe::egui;

use super::{App, EditField, error_label, run_idle, warn_label};
use nice_aimassist::{
    engine::{CapturePhase, Command, ModelPhase, RunPhase, State, Stats},
    hotkeys::Action,
    settings::{
        Confidence, FpsLimit, Interpolation, InterpolationPercent, MaxDelta, NewFrameWait,
        Sensitivity, Settings, TargetYOffset, VelocitySmoothing,
    },
};

const FIXED_INTERPOLATION_START: u16 = 50;

const STEPS: [(&str, i32); 4] = [("−10", -10), ("−1", -1), ("+1", 1), ("+10", 10)];

#[derive(Debug)]
struct Edit<T> {
    value: Option<T>,
    persist: bool,
    dirty: bool,
}

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

    fn drag_row<T>(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        field: EditField,
        value: T,
        range: RangeInclusive<T>,
        set: impl FnOnce(&mut Settings, T),
    ) where
        T: Copy + Into<i32> + TryFrom<i32>,
    {
        ui.label(label);
        let edit = drag_value(ui, value, range, self.dirty.contains(&field));
        self.apply_edit(field, edit, |app, value| {
            app.edit(|settings| set(settings, value));
        });
    }

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
                    self.edit(|settings| {
                        settings.interpolation = if auto {
                            Interpolation::Auto
                        } else {
                            Interpolation::Percent(InterpolationPercent::new(
                                FIXED_INTERPOLATION_START,
                            ))
                        };
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
                    app.edit(|settings| {
                        settings.interpolation =
                            Interpolation::Percent(InterpolationPercent::new(value));
                    });
                });
            }
        });
    }

    pub(super) fn draw_calibration(&mut self, ui: &mut egui::Ui, engine: &State, stats: &Stats) {
        ui.group(|ui| {
            ui.strong("Calibration");
            self.draw_calibration_values(ui);
            self.draw_blocked(ui, engine);
            draw_last_run(ui, stats);
            if let Some(error) = &engine.run.error {
                error_label(ui, error);
            }
        });
    }

    fn draw_calibration_values(&mut self, ui: &mut egui::Ui) {
        let edit = percent_row(
            ui,
            "Sensitivity X",
            self.settings.sensitivity_x.get(),
            Sensitivity::MIN..=Sensitivity::MAX,
            self.dirty.contains(&EditField::SensitivityX),
        );
        self.apply_edit(EditField::SensitivityX, edit, |app, value| {
            app.edit(|settings| settings.sensitivity_x = Sensitivity::new(value));
        });
        let edit = percent_row(
            ui,
            "Sensitivity Y",
            self.settings.sensitivity_y.get(),
            Sensitivity::MIN..=Sensitivity::MAX,
            self.dirty.contains(&EditField::SensitivityY),
        );
        self.apply_edit(EditField::SensitivityY, edit, |app, value| {
            app.edit(|settings| settings.sensitivity_y = Sensitivity::new(value));
        });
        ui.small("100% = 1× command scale, per axis");

        let edit = percent_row(
            ui,
            "Confidence",
            self.settings.confidence.get(),
            Confidence::MIN..=Confidence::MAX,
            self.dirty.contains(&EditField::Confidence),
        );
        self.apply_edit(EditField::Confidence, edit, |app, value| {
            app.edit(|settings| settings.confidence = Confidence::new(value));
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
            app.edit(|settings| settings.target_y_offset = TargetYOffset::new(value));
        });
        ui.small("Aim point inside the target box: 0% = top, 50% = center, 100% = bottom.");

        ui.horizontal(|ui| {
            self.drag_row(
                ui,
                "Max delta",
                EditField::MaxDelta,
                self.settings.max_delta.get(),
                0..=MaxDelta::MAX,
                |settings, value| settings.max_delta = MaxDelta::new(value),
            );
        });
        ui.small("Per-frame cap on movement length after sensitivity; 0 = unlimited.");

        self.draw_lead_values(ui);
    }

    fn draw_lead_values(&mut self, ui: &mut egui::Ui) {
        let edit = percent_row(
            ui,
            "Velocity smoothing",
            self.settings.velocity_smoothing.get(),
            VelocitySmoothing::MIN..=VelocitySmoothing::MAX,
            self.dirty.contains(&EditField::VelocitySmoothing),
        );
        self.apply_edit(EditField::VelocitySmoothing, edit, |app, value| {
            app.edit(|settings| settings.velocity_smoothing = VelocitySmoothing::new(value));
        });
        ui.small("Higher = steadier target speed, slower to react.");
    }

    fn draw_blocked(&mut self, ui: &mut egui::Ui, engine: &State) {
        if let Some(reason) = engine.blocked() {
            ui.horizontal(|ui| {
                ui.small(reason);
                if engine.capture.phase == CapturePhase::Failed
                    && ui.small_button("Retry capture").clicked()
                {
                    self.commands.send(Command::RetryCapture);
                }
            });
        }
    }

    pub(super) fn draw_continuous(&mut self, ui: &mut egui::Ui, engine: &State, stats: &Stats) {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.strong("Continuous");
                ui.separator();
                ui.label(match engine.run.phase {
                    RunPhase::Continuous => "● Running",
                    RunPhase::Idle => "Idle",
                });
            });
            ui.horizontal(|ui| {
                self.drag_row(
                    ui,
                    "FPS limit",
                    EditField::FpsLimit,
                    self.settings.fps_limit.get(),
                    FpsLimit::MIN..=FpsLimit::MAX,
                    |settings, value| settings.fps_limit = FpsLimit::new(value),
                );
                ui.small("Continuous only");
            });

            self.draw_new_frame_wait(ui);
            self.draw_low_latency(ui, engine);

            let hold_shortcuts = bindings_summary(&self.settings, Action::Hold);
            if hold_shortcuts.is_empty() {
                ui.small("No hold shortcut assigned.");
            } else {
                ui.small(format!("Hold: {hold_shortcuts}"));
            }
            self.draw_frame_stats(ui, stats);
        });
    }

    fn draw_new_frame_wait(&mut self, ui: &mut egui::Ui) {
        let mut wait = self.settings.new_frame_wait;
        ui.horizontal(|ui| {
            ui.label("New frame wait");
            let mut milliseconds = wait.get();
            ui.add(
                egui::DragValue::new(&mut milliseconds)
                    .range(NewFrameWait::MIN..=NewFrameWait::MAX)
                    .suffix(" ms"),
            )
            .on_hover_text(
                "Maximum wait per capture attempt. FPS limit still applies to new frames.",
            );
            wait = NewFrameWait::new(milliseconds);
        });
        if wait != self.settings.new_frame_wait {
            self.edit(|settings| settings.new_frame_wait = wait);
            self.persist();
        }
    }

    fn draw_low_latency(&mut self, ui: &mut egui::Ui, engine: &State) {
        let can_change_latency = run_idle(engine) && engine.model.phase != ModelPhase::Loading;
        ui.add_enabled_ui(can_change_latency, |ui| {
            let mut low_latency = self.settings.low_latency;
            if ui
                .checkbox(&mut low_latency, "Low-latency mode")
                .on_hover_text(
                    "Busy-waits for the GPU instead of sleeping. Lowers worst-case latency \
                     but keeps one CPU core fully busy while running. Reloads the model.",
                )
                .changed()
            {
                self.edit(|settings| settings.low_latency = low_latency);
                self.persist();
            }
        });
    }

    fn draw_frame_stats(&self, ui: &mut egui::Ui, stats: &Stats) {
        if stats.stages.sample_count == 0 {
            return;
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
}

fn draw_last_run(ui: &mut egui::Ui, stats: &Stats) {
    if stats.last_target.is_some() || stats.last_move.is_some() || stats.last_total_ms.is_some() {
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
}

fn drag_value<T>(ui: &mut egui::Ui, value: T, range: RangeInclusive<T>, dirty: bool) -> Edit<T>
where
    T: Copy + Into<i32> + TryFrom<i32>,
{
    let low: i32 = (*range.start()).into();
    let high: i32 = (*range.end()).into();
    let mut edited: i32 = value.into();
    let response = ui.add(
        egui::DragValue::new(&mut edited)
            .speed(1.0)
            .range(low..=high),
    );
    let mut edit = Edit {
        value: None,
        persist: false,
        dirty,
    };
    if response.changed() {
        edit.dirty = true;
        // The drag value is clamped to `range`, so the conversion back succeeds.
        edit.value = T::try_from(edited).ok();
    }
    if edit.dirty && (response.drag_stopped() || response.lost_focus()) {
        edit.dirty = false;
        edit.persist = true;
    }
    edit
}

fn percent_row<T>(
    ui: &mut egui::Ui,
    label: &str,
    value: T,
    range: RangeInclusive<T>,
    dirty: bool,
) -> Edit<T>
where
    T: Copy + Into<i32> + TryFrom<i32>,
{
    ui.horizontal(|ui| {
        ui.label(label);
        let mut edit = drag_value(ui, value, range.clone(), dirty);
        ui.label("%");
        let low: i32 = (*range.start()).into();
        let high: i32 = (*range.end()).into();
        let current: i32 = value.into();
        for (text, delta) in STEPS {
            if ui.button(text).clicked() {
                let stepped = (current + delta).clamp(low, high);
                edit = Edit {
                    value: T::try_from(stepped).ok(),
                    persist: true,
                    dirty: false,
                };
            }
        }
        edit
    })
    .inner
}

fn bindings_summary(settings: &Settings, action: Action) -> String {
    settings
        .bindings_for(action)
        .map(|binding| binding.shortcut.label())
        .collect::<Vec<_>>()
        .join(", ")
}
