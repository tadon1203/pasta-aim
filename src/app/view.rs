use eframe::egui;

use super::App;
use crate::{
    config::{Config, LogLevel},
    engine::{
        CapturePhase, EngineStatus, MakPhase, ModelPhase, RunPhase, cpu_fallback_nodes,
        provider_display_name,
    },
    graphics::DisplayInfo,
    hotkeys::{CaptureResult, capture_from_events, display_shortcut},
    inference::{EpAssignmentState, TensorPrecision, ep_assignment::EpAssignedNode},
    mak::PhysicalControllerStatus,
    metrics::{MetricSummary, PerformanceSnapshot},
    models,
    settings::{
        ConfidenceThreshold, FpsLimit, HotkeyAction, InjectionApi, OutputMode, Sensitivity,
        TargetYOffset,
    },
};

impl App {
    fn draw_model(&mut self, ui: &mut egui::Ui, status: &EngineStatus) {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.strong("Model");
                if status.model.phase == ModelPhase::Loading {
                    ui.spinner();
                    ui.label("Loading…");
                }
            });

            let selected_text = status
                .model
                .loading_name
                .as_ref()
                .or(status.model.active_name.as_ref())
                .or(self.config.model.as_ref())
                .cloned()
                .unwrap_or_else(|| "Select a model".to_owned());

            ui.horizontal(|ui| {
                let can_change =
                    status.model.phase != ModelPhase::Loading && status.run.phase == RunPhase::Idle;
                ui.add_enabled_ui(can_change, |ui| {
                    egui::ComboBox::from_id_salt("model_selector")
                        .selected_text(selected_text)
                        .width(330.0)
                        .show_ui(ui, |ui| {
                            let entries = self.models.entries().to_vec();
                            for entry in entries {
                                let active = status.model.active_name.as_deref()
                                    == Some(entry.name.as_str());
                                if ui.selectable_label(active, &entry.name).clicked() {
                                    self.select_model(entry.name);
                                }
                            }
                        });
                });

                if ui.button("Refresh").clicked() {
                    self.rescan_models();
                }
                if ui.button("Open models folder").clicked()
                    && let Err(error) = models::open_models_folder()
                {
                    self.model_catalog_error = Some(format!("{error:#}"));
                }
            });

            if self.models.is_empty() {
                ui.small("No .onnx models found in the models folder.");
            }
            if let Some((width, height)) = status.model.input_size {
                let precision = status.model.precision.map_or("", TensorPrecision::label);
                ui.small(format!("Active input: {width}×{height} {precision}"));
            }
            if let Some(error) = &status.model.error {
                ui.colored_label(ui.visuals().error_fg_color, format!("Model: {error}"));
            }
            if let Some(error) = &self.model_catalog_error {
                ui.colored_label(ui.visuals().error_fg_color, format!("Models: {error}"));
            }
        });
    }

    fn draw_devices(&mut self, ui: &mut egui::Ui, status: &EngineStatus) {
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
                                let active = status.capture.selected.as_ref().is_some_and(|selected| {
                                    selected.adapter_index == display.adapter_index
                                        && selected.output_index == display.output_index
                                });
                                if ui.selectable_label(active, display.label()).clicked() {
                                    self.select_display(display.adapter_index, display.output_index);
                                }
                            }
                        });
                });
                if ui.small_button("Refresh").clicked() {
                    self.rescan_displays();
                }
            });
            if let Some(error) = &status.graphics.catalog_error {
                ui.colored_label(ui.visuals().error_fg_color, format!("Displays: {error}"));
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
                        if matches!(status.mak.phase, MakPhase::Connecting | MakPhase::Reconnecting)
                        {
                            ui.spinner();
                        }
                    });
                    ui.end_row();

                    ui.label("Mouse route");
                    ui.label(route_label(status.mak.routes.mouse));
                    ui.end_row();

                    ui.label("Controller route");
                    if status.mak.routes.controllers.is_empty() {
                        ui.label("— Not routed");
                    } else {
                        let labels = status
                            .mak
                            .routes
                            .controllers
                            .iter()
                            .map(|kind| kind.label())
                            .collect::<Vec<_>>()
                            .join(", ");
                        ui.label(format!("● {labels}"));
                    }
                    ui.end_row();

                    ui.label("Physical controller");
                    ui.horizontal(|ui| {
                        ui.label(physical_controller_label(status.mak.physical_controller));
                        if status.mak.routes.controller_available()
                            && status.mak.phase == MakPhase::Connected
                            && status.run.phase == RunPhase::Idle
                            && ui.small_button("Check").clicked()
                        {
                            self.engine.probe_controller();
                        }
                    });
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

            if status.mak.physical_controller == PhysicalControllerStatus::Unavailable {
                ui.small(
                    "The controller route exists, but a physical snapshot could not be read. This can mean no controller sample is available or the firmware does not support physical snapshots.",
                );
            }

            ui.horizontal(|ui| match status.mak.phase {
                MakPhase::Connected => {
                    if ui.button("Refresh routes").clicked() {
                        self.engine.refresh_mak_routes();
                    }
                    if ui.button("Rescan device").clicked() {
                        self.engine.rescan_mak();
                    }
                }
                MakPhase::Connecting | MakPhase::Reconnecting => {
                    ui.add_enabled(false, egui::Button::new("Connecting…"));
                }
                MakPhase::Failed => {
                    if ui.button("Connect MAK").clicked() {
                        self.engine.rescan_mak();
                    }
                }
            });

            if let Some(error) = &status.mak.error {
                ui.colored_label(ui.visuals().error_fg_color, format!("MAK: {error}"));
            }
        });
    }

    fn draw_output(&mut self, ui: &mut egui::Ui, status: &EngineStatus) {
        ui.group(|ui| {
            ui.strong("Output");
            ui.horizontal(|ui| {
                if ui
                    .radio_value(&mut self.config.output_mode, OutputMode::Mouse, "Mouse")
                    .changed()
                {
                    self.engine.set_output(OutputMode::Mouse);
                    self.persist();
                }
                if ui
                    .radio_value(
                        &mut self.config.output_mode,
                        OutputMode::Controller,
                        "Controller",
                    )
                    .changed()
                {
                    self.engine.set_output(OutputMode::Controller);
                    self.persist();
                }
            });

            ui.label("Injection API");
            ui.horizontal(|ui| {
                if ui
                    .radio_value(
                        &mut self.config.injection_api,
                        InjectionApi::MakApi,
                        InjectionApi::MakApi.label(),
                    )
                    .changed()
                {
                    self.engine.set_injection_api(self.config.injection_api);
                    self.persist();
                }
                if ui
                    .radio_value(
                        &mut self.config.injection_api,
                        InjectionApi::KmApi,
                        InjectionApi::KmApi.label(),
                    )
                    .changed()
                {
                    self.engine.set_injection_api(self.config.injection_api);
                    self.persist();
                }
            });

            match self.config.output_mode {
                OutputMode::Mouse if !status.mak.routes.mouse => {
                    ui.colored_label(ui.visuals().warn_fg_color, "Waiting for a mouse route.");
                }
                OutputMode::Controller if !status.mak.routes.controller_available() => {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "Waiting for a controller route.",
                    );
                }
                OutputMode::Controller
                    if status.mak.physical_controller == PhysicalControllerStatus::Unavailable =>
                {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "Controller route is ready, but the physical controller could not be confirmed.",
                    );
                }
                _ => {}
            }
        });
    }

    fn draw_calibration(&mut self, ui: &mut egui::Ui, status: &EngineStatus) {
        ui.group(|ui| {
            ui.strong("Calibration");
            ui.horizontal(|ui| {
                ui.label("Sensitivity");
                let mut value = self.config.sensitivity.value();
                let response = ui.add(
                    egui::DragValue::new(&mut value)
                        .speed(1.0)
                        .range(Sensitivity::MIN..=Sensitivity::MAX),
                );
                if response.changed() {
                    self.sensitivity_dirty = true;
                    self.set_sensitivity(Sensitivity::new(value), false);
                }
                if self.sensitivity_dirty && (response.drag_stopped() || response.lost_focus()) {
                    self.sensitivity_dirty = false;
                    self.persist();
                }
                ui.label("%");

                for (label, delta) in [("−10", -10), ("−1", -1), ("+1", 1), ("+10", 10)] {
                    if ui.button(label).clicked() {
                        self.set_sensitivity(self.config.sensitivity.adjusted(delta), true);
                    }
                }
            });
            ui.small("100% = 1× command scale");

            ui.horizontal(|ui| {
                ui.label("Confidence");
                let mut value = self.config.confidence_threshold.value();
                let response = ui.add(
                    egui::DragValue::new(&mut value)
                        .speed(1.0)
                        .range(ConfidenceThreshold::MIN..=ConfidenceThreshold::MAX),
                );
                if response.changed() {
                    self.confidence_threshold_dirty = true;
                    self.set_confidence_threshold(ConfidenceThreshold::new(value), false);
                }
                if self.confidence_threshold_dirty
                    && (response.drag_stopped() || response.lost_focus())
                {
                    self.confidence_threshold_dirty = false;
                    self.persist();
                }
                ui.label("%");
                for (label, delta) in [("−10", -10), ("−1", -1), ("+1", 1), ("+10", 10)] {
                    if ui.button(label).clicked() {
                        self.set_confidence_threshold(
                            self.config.confidence_threshold.adjusted(delta),
                            true,
                        );
                    }
                }
            });
            ui.small("Minimum confidence required to accept a detection.");

            ui.horizontal(|ui| {
                ui.label("Target Y offset");
                let mut value = self.config.target_y_offset.value();
                let response = ui.add(
                    egui::DragValue::new(&mut value)
                        .speed(1.0)
                        .range(TargetYOffset::MIN..=TargetYOffset::MAX),
                );
                if response.changed() {
                    self.target_y_offset_dirty = true;
                    self.set_target_y_offset(TargetYOffset::new(i32::from(value)), false);
                }
                if self.target_y_offset_dirty && (response.drag_stopped() || response.lost_focus())
                {
                    self.target_y_offset_dirty = false;
                    self.persist();
                }
                ui.label("%");
                for (label, delta) in [("−10", -10), ("−1", -1), ("+1", 1), ("+10", 10)] {
                    if ui.button(label).clicked() {
                        self.set_target_y_offset(self.config.target_y_offset.adjusted(delta), true);
                    }
                }
            });
            ui.small("Relative to target height; positive values move downward.");

            let ready = status.ready_for(self.config.output_mode);
            let run_idle = status.run.phase == RunPhase::Idle;
            if ui
                .add_enabled(ready && run_idle, egui::Button::new("Run once"))
                .clicked()
            {
                self.engine.run_once();
            }

            let once_shortcuts = bindings_summary(&self.config, HotkeyAction::RunOnce);
            if !once_shortcuts.is_empty() {
                ui.small(format!("Shortcuts: {once_shortcuts}"));
            }

            if !ready && let Some(reason) = status.block_reason(self.config.output_mode) {
                ui.horizontal(|ui| {
                    ui.small(reason);
                    if status.capture.phase == CapturePhase::Failed
                        && ui.small_button("Retry capture").clicked()
                    {
                        self.engine.retry_capture();
                    }
                });
            }

            if status.run.last_target.is_some()
                || status.run.last_command.is_some()
                || status.run.last_total_ms.is_some()
            {
                ui.separator();
                ui.small("Last run");
                if let Some((dx, dy, _, _)) = status.run.last_target {
                    ui.label(format!("Target     x={dx:+.1} px   y={dy:+.1} px"));
                } else {
                    ui.label("Target     no detection");
                }
                if let Some((x, y, mode)) = status.run.last_command {
                    ui.label(format!(
                        "Command    x={x:+}   y={y:+}   via {}",
                        mode.label()
                    ));
                }
                if let Some(ms) = status.run.last_total_ms {
                    ui.label(format!("Pipeline   {ms:.2} ms"));
                }
            }

            if let Some(error) = &status.run.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        });
    }

    fn draw_continuous(&mut self, ui: &mut egui::Ui, status: &EngineStatus) {
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
                let mut value = self.config.fps_limit.value();
                let response = ui.add(
                    egui::DragValue::new(&mut value)
                        .speed(1.0)
                        .range(FpsLimit::MIN..=FpsLimit::MAX),
                );
                if response.changed() {
                    self.fps_limit_dirty = true;
                    self.set_fps_limit(FpsLimit::new(value), false);
                }
                if self.fps_limit_dirty && (response.drag_stopped() || response.lost_focus()) {
                    self.fps_limit_dirty = false;
                    self.persist();
                }
                ui.small("Continuous only");
            });

            let hold_shortcuts = bindings_summary(&self.config, HotkeyAction::ContinuousHold);
            if hold_shortcuts.is_empty() {
                ui.small("No hold shortcut assigned.");
            } else {
                ui.small(format!("Hold: {hold_shortcuts}"));
            }
            if status.performance.sample_count > 0 {
                if self.gpu_debug_at_startup {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "GPU debug is on: timings are inflated.",
                    );
                }
                ui.small(format!(
                    "{:.1} FPS / {} cap · pipeline {:.2}/{:.2} ms p50/p95 · inference {:.2}/{:.2} ms p50/p95",
                    status.performance.fps,
                    self.config.fps_limit.value(),
                    status.performance.total.p50_ms,
                    status.performance.total.p95_ms,
                    status.performance.inference.p50_ms,
                    status.performance.inference.p95_ms,
                ));
            }
        });
    }

    fn draw_hotkeys(&mut self, ui: &mut egui::Ui, status: &EngineStatus) {
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
                        self.capturing_hotkey.is_none() && status.run.phase == RunPhase::Idle;
                    if ui
                        .add_enabled(can_add, egui::Button::new("+ Add"))
                        .clicked()
                    {
                        self.begin_hotkey_capture(action);
                    }
                });
            }

            if let Some(error) = &self.hotkey_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        });
    }

    fn draw_diagnostics(&mut self, ui: &mut egui::Ui, status: &EngineStatus) {
        egui::CollapsingHeader::new("Diagnostics")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("Log level");
                    let mut level = self.config.diagnostics.level;
                    egui::ComboBox::from_id_salt("log_level_selector")
                        .selected_text(log_level_label(level))
                        .show_ui(ui, |ui| {
                            for option in [
                                LogLevel::Off,
                                LogLevel::Error,
                                LogLevel::Warn,
                                LogLevel::Info,
                                LogLevel::Debug,
                                LogLevel::Trace,
                            ] {
                                ui.selectable_value(&mut level, option, log_level_label(option));
                            }
                        });
                    if level != self.config.diagnostics.level {
                        self.config.diagnostics.level = level;
                        crate::logging::set_level(level);
                        self.persist();
                    }
                });
                ui.small("The selected level and more severe logs are written to the log file.");
                let mut gpu_debug = self.config.diagnostics.gpu_debug;
                if ui
                    .checkbox(
                        &mut gpu_debug,
                        "D3D12 debug layer + DRED (restart required)",
                    )
                    .changed()
                {
                    self.config.diagnostics.gpu_debug = gpu_debug;
                    self.persist();
                }
                ui.small("Slows inference by ~1-2 ms. Turn off when measuring performance.");
                if self.config.diagnostics.gpu_debug != self.gpu_debug_at_startup {
                    ui.colored_label(ui.visuals().warn_fg_color, "Restart to apply.");
                }
                ui.add_space(6.0);
                egui::Grid::new("diagnostics_state")
                    .num_columns(2)
                    .spacing([16.0, 5.0])
                    .show(ui, |ui| {
                        ui.label("GPU debug");
                        ui.label(if self.gpu_debug_at_startup {
                            "On"
                        } else {
                            "Off"
                        });
                        ui.end_row();

                        ui.label("Model state");
                        ui.label(model_phase_label(status.model.phase));
                        ui.end_row();

                        if let Some(model) = &status.model.active_name {
                            ui.label("Active model");
                            ui.label(model);
                            ui.end_row();
                        }

                        ui.label("Capture");
                        ui.label(capture_phase_label(status.capture.phase));
                        ui.end_row();

                        ui.label("Capture source");
                        ui.label(
                            status
                                .capture
                                .selected
                                .as_ref()
                                .map(DisplayInfo::label)
                                .unwrap_or_else(|| "Unavailable".to_owned()),
                        );
                        ui.end_row();

                        ui.label("Inference adapter");
                        ui.label(
                            status
                                .capture
                                .selected
                                .as_ref()
                                .map(|display| display.adapter_name.clone())
                                .unwrap_or_else(|| "Unavailable".to_owned()),
                        );
                        ui.end_row();

                        if let Some(assignment) = &status.model.ep_assignment {
                            ui.label("Actual assignment");
                            ui.label(actual_backend_label(assignment));
                            ui.end_row();
                        }

                        if let Some((width, height)) = status.capture.desktop_size {
                            ui.label("Desktop frame");
                            ui.label(format!("{width}×{height} BGRA8"));
                            ui.end_row();
                        }

                        ui.label("MAK state");
                        ui.label(mak_phase_label(status.mak.phase));
                        ui.end_row();

                        if let Some(port) = &status.mak.port {
                            ui.label("MAK port");
                            ui.label(port);
                            ui.end_row();
                        }
                        if let Some(firmware) = status.mak.firmware {
                            ui.label("MAK firmware");
                            ui.label(firmware.to_string());
                            ui.end_row();
                        }

                        ui.label("Physical controller");
                        ui.label(physical_controller_label(status.mak.physical_controller));
                        ui.end_row();

                        ui.label("Controller release");
                        ui.label(if status.mak.controller_release_pending {
                            "Pending zero-state handoff"
                        } else {
                            "Clear"
                        });
                        ui.end_row();

                        ui.label("XInput hotkey pad");
                        ui.label(
                            self.hotkeys
                                .xinput_slot()
                                .map(|slot| format!("Slot {slot}"))
                                .unwrap_or_else(|| "Not connected".to_owned()),
                        );
                        ui.end_row();

                        let release_latency = self.hotkeys.keyboard_release_latency();
                        ui.label("Keyboard release lag");
                        if release_latency.samples == 0 {
                            ui.label("No measured hold releases");
                        } else {
                            ui.label(format!(
                                "{:.1}/{:.1} ms p50/p95 · {} samples{}",
                                release_latency.p50_ms,
                                release_latency.p95_ms,
                                release_latency.samples,
                                if release_latency.assist_active {
                                    " · release assist active"
                                } else {
                                    ""
                                }
                            ));
                        }
                        ui.end_row();

                        ui.label("FPS limit");
                        ui.label(self.config.fps_limit.value().to_string());
                        ui.end_row();

                        ui.label("Confidence threshold");
                        ui.label(format!("{}%", self.config.confidence_threshold.value()));
                        ui.end_row();

                        if let Some((_, _, confidence, class_id)) = status.run.last_target {
                            ui.label("Last detection");
                            ui.label(format!("conf={confidence:.3}, class={class_id}"));
                            ui.end_row();
                        }
                    });

                draw_ep_assignment_diagnostics(ui, status);

                let release_latency = self.hotkeys.keyboard_release_latency();
                if release_latency.samples >= 8 && release_latency.p95_ms > 25.0 {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!(
                            "Keyboard Hold release lag is high: {:.1} ms p95",
                            release_latency.p95_ms
                        ),
                    );
                }

                if let Some(error) = &status.capture.error {
                    ui.colored_label(ui.visuals().error_fg_color, format!("Capture: {error}"));
                    if ui.button("Retry capture").clicked() {
                        self.engine.retry_capture();
                    }
                }

                ui.add_space(6.0);
                ui.strong("Performance");
                draw_performance(ui, &status.performance);

                ui.add_space(8.0);
                if ui.button("Copy diagnostics").clicked() {
                    ui.ctx().copy_text(self.diagnostics_text(status));
                }
            });
    }

    fn diagnostics_text(&self, status: &EngineStatus) -> String {
        let display = status
            .capture
            .selected
            .as_ref()
            .map(DisplayInfo::label)
            .unwrap_or_else(|| "Unavailable".to_owned());
        let model = status
            .model
            .active_name
            .clone()
            .unwrap_or_else(|| "None".to_owned());
        let input = status
            .model
            .input_size
            .map(|(w, h)| match status.model.precision {
                Some(precision) => format!("{w}x{h} {}", precision.label()),
                None => format!("{w}x{h}"),
            })
            .unwrap_or_else(|| "Unknown".to_owned());
        let controller_routes = status
            .mak
            .routes
            .controllers
            .iter()
            .map(|kind| kind.label())
            .collect::<Vec<_>>()
            .join(", ");
        let mak_routes = format!(
            "mouse={}, controllers=[{}]",
            status.mak.routes.mouse, controller_routes
        );
        let xinput = self
            .hotkeys
            .xinput_slot()
            .map(|slot| format!("slot {slot}"))
            .unwrap_or_else(|| "not connected".to_owned());
        let warning = self.config_load_warning.as_deref().unwrap_or("none");
        let save_error = self.config_save_error.as_deref().unwrap_or("none");
        let keyboard_release = self.hotkeys.keyboard_release_latency();
        format!(
            "nice-aimassist {}\nmodel: {model}\ninput: {input}\ninference adapter: {}\n{}\ncapture: {display}\nmak: {} · port={} · firmware={}\nmak routes: {mak_routes}\ncontroller release pending: {}\nphysical controller: {}\nxinput hotkey controller: {xinput}\nkeyboard release lag p50/p95: {:.1}/{:.1} ms ({} samples, assist={})\nfps limit: {}\nfresh/admitted FPS: {:.1}/{:.1}\nadmission drop: {:.1}%\nperformance samples: {}\npipeline p50/p95/p99: {:.2}/{:.2}/{:.2} ms\nacquire wait p50/p95: {:.2}/{:.2} ms\ncapture p50/p95: {:.2}/{:.2} ms\ncopy submit p50/p95: {:.2}/{:.2} ms\nGPU copy p50/p95: {:.2}/{:.2} ms\nGPU preprocess p50/p95: {:.2}/{:.2} ms\nqueue age p50/p95: {:.2}/{:.2} ms\ninference p50/p95: {:.2}/{:.2} ms\nGPU slot drops: {} · mailbox overwrites: {} · slots in flight: {}\ncausal discards: {}\nconfig warning: {warning}\nconfig save error: {save_error}",
            env!("CARGO_PKG_VERSION"),
            status
                .capture
                .selected
                .as_ref()
                .map(|display| display.adapter_name.as_str())
                .unwrap_or("Unavailable"),
            ep_assignment_copy_text(status),
            mak_phase_label(status.mak.phase),
            status.mak.port.as_deref().unwrap_or("unknown"),
            status
                .mak
                .firmware
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            status.mak.controller_release_pending,
            physical_controller_label(status.mak.physical_controller),
            keyboard_release.p50_ms,
            keyboard_release.p95_ms,
            keyboard_release.samples,
            keyboard_release.assist_active,
            self.config.fps_limit.value(),
            status.performance.capture_source.fresh_fps,
            status.performance.capture_source.admitted_fps,
            status.performance.capture_source.admission_drop_ratio * 100.0,
            status.performance.sample_count,
            status.performance.total.p50_ms,
            status.performance.total.p95_ms,
            status.performance.total.p99_ms,
            status.performance.acquire_wait.p50_ms,
            status.performance.acquire_wait.p95_ms,
            status.performance.capture.p50_ms,
            status.performance.capture.p95_ms,
            status.performance.copy_submit.p50_ms,
            status.performance.copy_submit.p95_ms,
            status.performance.gpu_copy.p50_ms,
            status.performance.gpu_copy.p95_ms,
            status.performance.preprocess.p50_ms,
            status.performance.preprocess.p95_ms,
            status.performance.queue_age.p50_ms,
            status.performance.queue_age.p95_ms,
            status.performance.inference.p50_ms,
            status.performance.inference.p95_ms,
            status.performance.capture_source.gpu_slot_busy_drops,
            status.performance.capture_source.mailbox_overwrites,
            status.performance.capture_source.slots_in_flight,
            status.performance.causal_discards,
        )
    }

    fn draw_hotkey_capture(&mut self, ctx: &egui::Context) {
        let Some(action) = self.capturing_hotkey else {
            return;
        };

        if !self.hotkey_capture_armed {
            // Do not capture the Enter/Space key that may have activated `+ Add`
            // with keyboard navigation. Capture begins on the next egui pass.
            self.hotkey_capture_armed = true;
            ctx.request_repaint();
        } else if let Some(shortcut) = self.hotkeys.poll_captured_xinput() {
            self.add_hotkey(action, shortcut);
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
                            self.add_hotkey(action, hotkey.to_string());
                        }
                        Err(error) => self.hotkey_capture_error = Some(error),
                    }
                }
            }
        }

        if self.capturing_hotkey.is_none() {
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
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.add_space(8.0);
                if ui.button("Cancel").clicked() {
                    self.cancel_hotkey_capture();
                }
            });
        if !open && self.capturing_hotkey.is_some() {
            self.cancel_hotkey_capture();
        }
    }
}

fn log_level_label(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Off => "Off",
        LogLevel::Error => "Error",
        LogLevel::Warn => "Warn",
        LogLevel::Info => "Info",
        LogLevel::Debug => "Debug",
        LogLevel::Trace => "Trace",
    }
}

fn actual_backend_label(state: &EpAssignmentState) -> String {
    match state {
        EpAssignmentState::Unavailable(_) => "Unavailable".to_owned(),
        EpAssignmentState::Available(report) => {
            let names = report
                .providers
                .iter()
                .filter(|provider| !provider.nodes.is_empty())
                .map(provider_display_name)
                .collect::<Vec<_>>();
            if names.is_empty() {
                "No assigned nodes".to_owned()
            } else {
                names.join(" + ")
            }
        }
    }
}

fn draw_ep_assignment_diagnostics(ui: &mut egui::Ui, status: &EngineStatus) {
    let Some(state) = &status.model.ep_assignment else {
        return;
    };
    ui.add_space(6.0);
    ui.strong("Execution");
    match state {
        EpAssignmentState::Unavailable(error) => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Assignment info unavailable: {error}"),
            );
        }
        EpAssignmentState::Available(report) => {
            ui.label(format!("Backend: {}", actual_backend_label(state)));
            ui.label(format!("Assigned nodes: {}", report.total_nodes));
            if let Some(summary) = &status.model.ep_assignment_summary {
                for (name, nodes, subgraphs) in [
                    (
                        "DirectML",
                        summary.directml_nodes,
                        summary.directml_subgraphs,
                    ),
                    ("CPU", summary.cpu_nodes, summary.cpu_subgraphs),
                ] {
                    let percent = if summary.total_nodes == 0 {
                        0.0
                    } else {
                        nodes as f64 / summary.total_nodes as f64 * 100.0
                    };
                    ui.label(format!(
                        "{name}: {nodes} nodes · {subgraphs} subgraphs · {percent:.1}%"
                    ));
                }
            }
            for provider in report.providers.iter().filter(|provider| {
                provider.name != "DmlExecutionProvider" && provider.name != "CPUExecutionProvider"
            }) {
                let percent = if report.total_nodes == 0 {
                    0.0
                } else {
                    provider.nodes.len() as f64 / report.total_nodes as f64 * 100.0
                };
                ui.label(format!(
                    "{}: {} nodes · {} subgraphs · {:.1}%",
                    provider_display_name(provider),
                    provider.nodes.len(),
                    provider.subgraph_count,
                    percent
                ));
            }
            ui.small("Node percentages describe assignment counts, not GPU work or performance.");

            let cpu_nodes = cpu_fallback_nodes(state);
            if cpu_nodes.is_empty() {
                ui.label("CPU fallback: None");
            } else {
                egui::CollapsingHeader::new(format!("CPU fallback nodes ({})", cpu_nodes.len()))
                    .default_open(false)
                    .show(ui, |ui| draw_assigned_nodes(ui, cpu_nodes));
            }

            egui::CollapsingHeader::new("All EP assignments")
                .default_open(false)
                .show(ui, |ui| {
                    for provider in &report.providers {
                        ui.strong(format!(
                            "{} ({})",
                            provider_display_name(provider),
                            provider.name
                        ));
                        draw_assigned_nodes(ui, &provider.nodes);
                    }
                });
        }
    }
}

fn draw_assigned_nodes(ui: &mut egui::Ui, nodes: &[EpAssignedNode]) {
    for node in nodes {
        ui.horizontal_wrapped(|ui| {
            ui.monospace(&node.op_type);
            ui.label(&node.name);
            if !node.domain.is_empty() {
                ui.small(format!("domain: {}", node.domain));
            }
        });
    }
}

fn ep_assignment_copy_text(status: &EngineStatus) -> String {
    let Some(state) = &status.model.ep_assignment else {
        return "Actual assignment: not available (no active model session)".to_owned();
    };
    match state {
        EpAssignmentState::Unavailable(error) => format!("Assignment info unavailable: {error}"),
        EpAssignmentState::Available(report) => {
            let mut lines = vec![
                format!("Inference backend: {}", actual_backend_label(state)),
                format!("Total assigned nodes: {}", report.total_nodes),
            ];
            for provider in &report.providers {
                lines.push(format!(
                    "{}: {} nodes / {} subgraphs",
                    provider.name,
                    provider.nodes.len(),
                    provider.subgraph_count
                ));
            }
            let cpu_nodes = cpu_fallback_nodes(state);
            if cpu_nodes.is_empty() {
                lines.push("CPU fallback: None".to_owned());
            } else {
                lines.push("CPU fallback:".to_owned());
                for node in cpu_nodes {
                    let domain = if node.domain.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", node.domain)
                    };
                    lines.push(format!("- {}: {}{}", node.op_type, node.name, domain));
                }
            }
            lines.join("\n")
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
                                ui.colored_label(ui.visuals().warn_fg_color, label);
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
                        ui.colored_label(ui.visuals().warn_fg_color, warning);
                    }
                    if let Some(error) = &self.config_save_error {
                        ui.add_space(6.0);
                        ui.colored_label(ui.visuals().error_fg_color, error);
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

fn draw_performance(ui: &mut egui::Ui, performance: &PerformanceSnapshot) {
    ui.label(format!(
        "Fresh FPS {:.1} · Admitted FPS {:.1} · FPS admission drop {:.1}%",
        performance.capture_source.fresh_fps,
        performance.capture_source.admitted_fps,
        performance.capture_source.admission_drop_ratio * 100.0,
    ));
    ui.small(format!(
        "Accumulated {:.2} · GPU slot drops {} · Mailbox overwrites {} · Slots in flight {} · Causal discards {}",
        performance.capture_source.mean_accumulated_frames,
        performance.capture_source.gpu_slot_busy_drops,
        performance.capture_source.mailbox_overwrites,
        performance.capture_source.slots_in_flight,
        performance.causal_discards,
    ));
    if performance.sample_count == 0 {
        if let Some(cold) = performance.cold_run {
            ui.label(format!("Cold run: {:.2} ms total", cold.total_ms));
        } else {
            ui.label("No performance samples yet");
        }
        return;
    }

    ui.label(format!(
        "{:.1} FPS · {} samples",
        performance.fps, performance.sample_count
    ));
    egui::Grid::new("performance_grid")
        .num_columns(4)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            ui.label("");
            ui.strong("p50");
            ui.strong("p95");
            ui.strong("p99");
            ui.end_row();
            metric_row(ui, "Pipeline", performance.total);
            metric_row(
                ui,
                "Present → GPU submit",
                performance.present_to_gpu_submit,
            );
            metric_row(
                ui,
                "Present → inference",
                performance.present_to_inference_start,
            );
            metric_row(ui, "DXGI acquire wait", performance.acquire_wait);
            metric_row(ui, "Capture", performance.capture);
            metric_row(ui, "GPU copy submit", performance.copy_submit);
            metric_row(ui, "GPU copy", performance.gpu_copy);
            metric_row(ui, "GPU preprocess", performance.preprocess);
            metric_row(ui, "Queue age", performance.queue_age);
            metric_row(ui, "Inference", performance.inference);
            metric_row(ui, "Postprocess", performance.postprocess);
            metric_row(ui, "MAK write", performance.mak_write);
            metric_row(ui, "Cadence", performance.cadence);
        });

    if let Some(cold) = performance.cold_run {
        ui.small(format!("Cold run total: {:.2} ms", cold.total_ms));
    }
    ui.small("Pipeline: DXGI LastPresentTime → MAK write completion");
}

fn metric_row(ui: &mut egui::Ui, name: &str, metric: MetricSummary) {
    ui.label(name);
    ui.label(format!("{:.2} ms", metric.p50_ms));
    ui.label(format!("{:.2} ms", metric.p95_ms));
    ui.label(format!("{:.2} ms", metric.p99_ms));
    ui.end_row();
}

fn model_phase_label(phase: ModelPhase) -> &'static str {
    match phase {
        ModelPhase::Empty => "No active model",
        ModelPhase::Loading => "Loading",
        ModelPhase::Ready => "Ready",
    }
}

fn capture_phase_label(phase: CapturePhase) -> &'static str {
    match phase {
        CapturePhase::Initializing => "Initializing",
        CapturePhase::Ready => "Ready",
        CapturePhase::Failed => "Failed",
    }
}

fn mak_phase_label(phase: MakPhase) -> &'static str {
    match phase {
        MakPhase::Connecting => "Connecting",
        MakPhase::Connected => "Connected",
        MakPhase::Reconnecting => "Reconnecting",
        MakPhase::Failed => "Disconnected",
    }
}

fn mak_phase_mark(phase: MakPhase) -> &'static str {
    match phase {
        MakPhase::Connected => "●",
        MakPhase::Connecting | MakPhase::Reconnecting => "◌",
        MakPhase::Failed => "○",
    }
}

fn route_label(available: bool) -> &'static str {
    if available {
        "● Ready"
    } else {
        "— Not routed"
    }
}

fn physical_controller_label(status: PhysicalControllerStatus) -> &'static str {
    match status {
        PhysicalControllerStatus::NotRouted => "— Not routed",
        PhysicalControllerStatus::Checking => "◌ Checking…",
        PhysicalControllerStatus::Connected => "● Connected",
        PhysicalControllerStatus::Unavailable => "○ Unavailable / not confirmed",
        PhysicalControllerStatus::Unknown => "? Unknown",
    }
}
