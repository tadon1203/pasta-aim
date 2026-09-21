use eframe::egui;

use crate::{
    config::{Config, HotkeyAction, HotkeyBinding, OutputMode, Sensitivity},
    engine::{CapturePhase, Engine, EngineStatus, MakPhase, ModelPhase, RunPhase},
    hotkeys::{
        CaptureResult, HotkeyRegistry, canonical_shortcut, capture_from_events, display_shortcut,
    },
    mak_output::PhysicalControllerStatus,
    metrics::{MetricSummary, PerformanceSnapshot},
    models::{self, ModelCatalog},
};

pub struct App {
    config: Config,
    engine: Engine,
    models: ModelCatalog,
    hotkeys: HotkeyRegistry,
    capturing_hotkey: Option<HotkeyAction>,
    hotkey_capture_armed: bool,
    hotkey_capture_error: Option<String>,
    hotkey_error: Option<String>,
    model_catalog_error: Option<String>,
    config_error: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load();
        let (models, model_catalog_error) = match ModelCatalog::scan() {
            Ok(catalog) => (catalog, None),
            Err(error) => (ModelCatalog::default(), Some(format!("{error:#}"))),
        };

        let engine = Engine::new(&config, cc.egui_ctx.clone());
        let mut hotkeys = HotkeyRegistry::new(engine.control(), cc.egui_ctx.clone());
        let hotkey_error = hotkeys.replace_bindings(&config.hotkeys).err();

        let app = Self {
            config,
            engine,
            models,
            hotkeys,
            capturing_hotkey: None,
            hotkey_capture_armed: false,
            hotkey_capture_error: None,
            hotkey_error,
            model_catalog_error,
            config_error: None,
        };

        if app.config.model.is_none()
            && let Some(entry) = app.models.sole_model()
        {
            app.engine
                .load_model(entry.name.clone(), entry.path.clone());
        }

        app
    }

    fn persist(&mut self) {
        match self.config.save() {
            Ok(()) => self.config_error = None,
            Err(error) => self.config_error = Some(format!("Settings save failed: {error:#}")),
        }
    }

    fn commit_loaded_model(&mut self, status: &EngineStatus) {
        let Some(active) = &status.model.active_name else {
            return;
        };
        if self.config.model.as_ref() != Some(active) {
            self.config.model = Some(active.clone());
            self.persist();
        }
    }

    fn rescan_models(&mut self) {
        match ModelCatalog::scan() {
            Ok(catalog) => {
                self.models = catalog;
                self.model_catalog_error = None;
                if self.config.model.is_none()
                    && let Some(entry) = self.models.sole_model()
                {
                    self.engine
                        .load_model(entry.name.clone(), entry.path.clone());
                }
            }
            Err(error) => self.model_catalog_error = Some(format!("{error:#}")),
        }
    }

    fn select_model(&mut self, name: String) {
        if let Some(path) = self.models.path_for(&name) {
            self.engine.load_model(name, path);
        }
    }

    fn set_sensitivity(&mut self, sensitivity: Sensitivity, persist: bool) {
        self.config.sensitivity = sensitivity;
        self.engine.set_sensitivity(sensitivity);
        if persist {
            self.persist();
        }
    }

    fn begin_hotkey_capture(&mut self, action: HotkeyAction) {
        self.capturing_hotkey = Some(action);
        self.hotkey_capture_armed = false;
        self.hotkey_capture_error = None;
        self.hotkeys.suspend();
    }

    fn cancel_hotkey_capture(&mut self) {
        self.capturing_hotkey = None;
        self.hotkey_capture_armed = false;
        self.hotkey_capture_error = None;
        self.hotkeys.resume();
    }

    fn add_hotkey(&mut self, action: HotkeyAction, shortcut: String) {
        let shortcut = match canonical_shortcut(&shortcut) {
            Ok(shortcut) => shortcut,
            Err(error) => {
                self.hotkey_capture_error = Some(error);
                return;
            }
        };

        let mut candidate = self.config.clone();
        candidate.add_binding(HotkeyBinding { action, shortcut });
        match self.hotkeys.replace_bindings(&candidate.hotkeys) {
            Ok(()) => {
                self.config = candidate;
                self.hotkey_error = None;
                self.persist();
                self.cancel_hotkey_capture();
            }
            Err(error) => self.hotkey_capture_error = Some(error),
        }
    }

    fn remove_hotkey(&mut self, action: HotkeyAction, shortcut: &str) {
        let mut candidate = self.config.clone();
        candidate.remove_binding(action, shortcut);
        self.hotkeys.suspend();
        let result = self.hotkeys.replace_bindings(&candidate.hotkeys);
        self.hotkeys.resume();
        match result {
            Ok(()) => {
                self.config = candidate;
                self.hotkey_error = None;
                self.persist();
            }
            Err(error) => self.hotkey_error = Some(error),
        }
    }

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
                ui.small(format!("Active input: {width}×{height}"));
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
                    self.set_sensitivity(Sensitivity::new(value), false);
                }
                if response.drag_stopped() || response.lost_focus() {
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
            let hold_shortcuts = bindings_summary(&self.config, HotkeyAction::ContinuousHold);
            if hold_shortcuts.is_empty() {
                ui.small("No hold shortcut assigned.");
            } else {
                ui.small(format!("Hold: {hold_shortcuts}"));
            }
            if status.performance.sample_count > 0 {
                ui.small(format!(
                    "{:.1} FPS · pipeline {:.2}/{:.2} ms p50/p95 · inference {:.2}/{:.2} ms p50/p95",
                    status.performance.fps,
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
                egui::Grid::new("diagnostics_state")
                    .num_columns(2)
                    .spacing([16.0, 5.0])
                    .show(ui, |ui| {
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
                        ui.label("DXGI adapter 0 / output 0");
                        ui.end_row();

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

                        if let Some((_, _, confidence, class_id)) = status.run.last_target {
                            ui.label("Last detection");
                            ui.label(format!("conf={confidence:.3}, class={class_id}"));
                            ui.end_row();
                        }
                    });

                if let Some(error) = &status.capture.error {
                    ui.colored_label(ui.visuals().error_fg_color, format!("Capture: {error}"));
                    if ui.button("Retry capture").clicked() {
                        self.engine.retry_capture();
                    }
                }

                ui.add_space(6.0);
                ui.strong("Performance");
                draw_performance(ui, &status.performance);
            });
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
                ui.label("Press a key or key combination.");
                ui.small("Esc cancels. Ctrl, Shift, and Alt can be used as modifiers.");
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
        }

        let status = self.engine.status();
        self.commit_loaded_model(&status);

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

                    if let Some(error) = &self.config_error {
                        ui.add_space(6.0);
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                });
        });

        self.draw_hotkey_capture(ui.ctx());
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.config.save();
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
        .num_columns(3)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            ui.label("");
            ui.strong("p50");
            ui.strong("p95");
            ui.end_row();
            metric_row(ui, "Pipeline", performance.total);
            metric_row(ui, "Capture", performance.capture);
            metric_row(ui, "Preprocess", performance.preprocess);
            metric_row(ui, "Inference", performance.inference);
            metric_row(ui, "Postprocess", performance.postprocess);
            metric_row(ui, "Output submit", performance.output);
            metric_row(ui, "Cadence", performance.cadence);
        });

    if let Some(cold) = performance.cold_run {
        ui.small(format!("Cold run total: {:.2} ms", cold.total_ms));
    }
}

fn metric_row(ui: &mut egui::Ui, name: &str, metric: MetricSummary) {
    ui.label(name);
    ui.label(format!("{:.2} ms", metric.p50_ms));
    ui.label(format!("{:.2} ms", metric.p95_ms));
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
