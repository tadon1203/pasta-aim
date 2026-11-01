mod devices;
mod diagnostics;
mod hotkeys;
mod model;
mod run;

use std::{collections::HashSet, fmt, sync::Arc};

use eframe::egui;
use nice_aimassist::{
    channel::{self, Message, Receiver, Sender},
    engine::{Command, Engine, OutputPhase, RunPhase, State, Stats},
    hotkeys::{self as hotkey_thread, Action, Event, Hotkey, Hotkeys, Shortcut},
    output::Device,
    settings::{self, Display, Loaded, Settings},
};

pub struct App {
    settings: Settings,
    /// `gpu_debug` as applied at process start; the toggle only takes effect on restart.
    gpu_debug_at_startup: bool,
    /// Stops the engine thread when the app drops it.
    _engine: Engine,
    /// Sends to the engine thread.
    commands: Sender<Command>,
    /// The `.onnx` file names in `models/`.
    models: Vec<String>,
    /// `None` when the hotkeys thread could not start; `hotkey_error` says why.
    hotkeys: Option<Hotkeys>,
    /// What the engine thread and the hotkeys thread send to the UI.
    messages: Receiver<Message>,
    /// The engine state and the statistics, as they last arrived.
    state: Arc<State>,
    stats: Stats,
    /// The `XInput` controller in use, as the hotkeys thread last reported it.
    controller_slot: Option<u32>,
    /// The action of the binding that the user is adding. Some while the dialog is open.
    hotkey_capture: Option<Action>,
    hotkey_capture_error: Option<String>,
    hotkey_error: Option<String>,
    model_catalog_error: Option<String>,
    settings_warning: Option<String>,
    /// The settings file could not be read at startup; the next save backs it up first.
    settings_unusable: bool,
    settings_save_error: Option<String>,
    /// Value fields edited since they were last saved (see `run::drag_value`).
    dirty: HashSet<EditField>,
}

/// A numeric setting edited through a widget that saves once the edit ends.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum EditField {
    Interpolation,
    Sensitivity,
    Confidence,
    TargetYOffset,
    MaxDelta,
    FpsLimit,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, loaded: Loaded) -> Self {
        let settings = loaded.settings;
        let gpu_debug_at_startup = settings.gpu_debug;
        let settings_warning = loaded.warning;
        let settings_unusable = loaded.unusable;
        tracing::info!(model = ?settings.model, sensitivity = settings.sensitivity.get(), fps_limit = settings.fps_limit.get(), "application initialized with settings");
        let (models, model_catalog_error) = match settings::models() {
            Ok(names) => (names, None),
            Err(error) => {
                tracing::error!(error = %format_args!("{error:#}"), "initial model scan failed");
                (Vec::new(), Some(format!("{error:#}")))
            }
        };

        let (ui, messages) = channel::channel({
            let ctx = cc.egui_ctx.clone();
            move || {
                if ctx.input(|input| input.viewport().minimized) != Some(true) {
                    ctx.request_repaint();
                }
            }
        });
        let engine = Engine::spawn(&settings, ui.clone());
        let commands = engine.sender().clone();
        let (hotkeys, hotkey_error) = match Hotkeys::spawn(&settings.hotkeys, ui, commands.clone())
        {
            Ok(hotkeys) => (Some(hotkeys), None),
            Err(error) => {
                tracing::error!(error = %format_args!("{error:#}"), "hotkeys thread did not start");
                (None, Some(format!("{error:#}")))
            }
        };

        let app = Self {
            settings,
            gpu_debug_at_startup,
            _engine: engine,
            commands,
            models,
            hotkeys,
            messages,
            state: Arc::new(State::default()),
            stats: Stats::default(),
            controller_slot: None,
            hotkey_capture: None,
            hotkey_capture_error: None,
            hotkey_error,
            model_catalog_error,
            settings_warning,
            settings_unusable,
            settings_save_error: None,
            dirty: HashSet::new(),
        };

        // The initial scan is the sole automatic-load source.
        let mut app = app;
        app.request_sole_model_load();

        app
    }

    fn persist(&mut self) {
        match self.settings.save(self.settings_unusable) {
            Ok(()) => {
                tracing::debug!("settings persistence succeeded");
                self.settings_save_error = None;
                self.settings_warning = None;
                self.settings_unusable = false;
            }
            Err(error) => {
                tracing::error!(error = %format_args!("{error:#}"), "settings persistence failed");
                self.settings_save_error = Some(format!("Settings save failed: {error:#}"));
            }
        }
    }

    fn rescan_models(&mut self) {
        tracing::debug!("rescanning models from UI");
        match settings::models() {
            Ok(names) => {
                self.models = names;
                self.model_catalog_error = None;
                self.request_sole_model_load();
            }
            Err(error) => {
                tracing::error!(error = %format_args!("{error:#}"), "model rescan failed");
                self.model_catalog_error = Some(format!("{error:#}"));
            }
        }
    }

    fn select_model(&mut self, name: String) {
        tracing::debug!(model = %name, "model selected from UI");
        self.commands.send(Command::LoadModel(name));
    }

    /// Loads the only model in `models/` when no model is saved. The engine reports the load, so it is saved.
    fn request_sole_model_load(&mut self) {
        if self.settings.model.is_none()
            && let [name] = self.models.as_slice()
        {
            self.commands.send(Command::LoadModel(name.clone()));
        }
    }

    fn rescan_displays(&self) {
        tracing::debug!("display rescan requested from UI");
        self.commands.send(Command::RescanDisplays);
    }

    fn select_display(&self, target: Display) {
        tracing::debug!(?target, "display selected from UI");
        self.commands.send(Command::SelectDisplay(target));
    }

    /// Applies a change to the settings and sends all of them: the engine applies what differs. A drag
    /// or a click is saved by the caller with `persist`.
    fn edit(&mut self, change: impl FnOnce(&mut Settings)) {
        change(&mut self.settings);
        self.commands.send(Command::Settings(self.settings.clone()));
    }

    /// Applies what the other threads sent since the last frame.
    fn process_messages(&mut self) {
        while let Ok(message) = self.messages.try_recv() {
            match message {
                Message::State(state) => self.state = state,
                Message::Stats(stats) => self.stats = stats,
                Message::ModelLoaded(name) => {
                    tracing::debug!(model = %name, "model loaded");
                    self.settings.model = Some(name);
                    self.persist();
                }
                Message::DisplayChanged(selected) => {
                    tracing::debug!(?selected, "display changed");
                    self.settings.display = selected;
                    self.persist();
                }
                Message::Hotkeys(Event::Controller(slot)) => self.controller_slot = slot,
                Message::Hotkeys(Event::Recorded(shortcut)) => {
                    if let Some(action) = self.hotkey_capture {
                        self.add_hotkey(action, shortcut);
                    }
                }
                Message::Hotkeys(Event::RecordCancelled) => {
                    self.hotkey_capture = None;
                    self.hotkey_capture_error = None;
                }
            }
        }
    }

    /// Replaces the bindings in the hotkeys thread. The error is text for the UI.
    fn apply_hotkeys(&self, hotkeys: &[Hotkey]) -> Result<(), String> {
        let Some(running) = &self.hotkeys else {
            return Err("Hotkeys are not running".to_owned());
        };
        let bindings = hotkey_thread::bindings(hotkeys).map_err(|error| format!("{error:#}"))?;
        running
            .sender()
            .send(hotkey_thread::Command::SetBindings(bindings));
        Ok(())
    }

    fn begin_hotkey_capture(&mut self, action: Action) {
        tracing::debug!(?action, "hotkey capture started");
        self.hotkey_capture = Some(action);
        self.hotkey_capture_error = None;
        if let Some(hotkeys) = &self.hotkeys {
            hotkeys.sender().send(hotkey_thread::Command::Record);
        }
    }

    fn cancel_hotkey_capture(&mut self) {
        tracing::debug!("hotkey capture cancelled or completed");
        self.hotkey_capture = None;
        self.hotkey_capture_error = None;
        if let Some(hotkeys) = &self.hotkeys {
            hotkeys.sender().send(hotkey_thread::Command::CancelRecord);
        }
    }

    fn add_hotkey(&mut self, action: Action, shortcut: Shortcut) {
        tracing::debug!(?action, %shortcut, "adding hotkey binding");
        let mut candidate = self.settings.clone();
        candidate.add_binding(Hotkey { action, shortcut });
        match self.apply_hotkeys(&candidate.hotkeys) {
            Ok(()) => {
                self.settings = candidate;
                self.hotkey_error = None;
                self.persist();
                self.cancel_hotkey_capture();
            }
            Err(error) => {
                // The recording has ended. Record again, so that the user can choose another shortcut.
                tracing::warn!(%error, "hotkey binding was rejected");
                self.hotkey_capture_error = Some(error);
                if let Some(hotkeys) = &self.hotkeys {
                    hotkeys.sender().send(hotkey_thread::Command::Record);
                }
            }
        }
    }

    fn remove_hotkey(&mut self, action: Action, shortcut: Shortcut) {
        tracing::debug!(?action, %shortcut, "removing hotkey binding");
        let mut candidate = self.settings.clone();
        candidate.remove_binding(action, shortcut);
        match self.apply_hotkeys(&candidate.hotkeys) {
            Ok(()) => {
                self.settings = candidate;
                self.hotkey_error = None;
                self.persist();
            }
            Err(error) => {
                tracing::warn!(%error, "hotkey removal failed");
                self.hotkey_error = Some(error);
            }
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

        self.process_messages();
        let engine = Arc::clone(&self.state);
        let stats = self.stats;

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("nice-aimassist");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let ready = engine.blocked().is_none();
                            let label = if ready {
                                "Ready".to_owned()
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
                    self.draw_devices(ui, &engine);
                    ui.add_space(6.0);
                    self.draw_model(ui, &engine);
                    ui.add_space(6.0);
                    self.draw_output(ui, &engine);
                    ui.add_space(6.0);
                    self.draw_calibration(ui, &engine, &stats);
                    ui.add_space(6.0);
                    self.draw_continuous(ui, &engine, &stats);
                    ui.add_space(6.0);
                    self.draw_hotkeys(ui, &engine);
                    ui.add_space(6.0);
                    self.draw_diagnostics(ui, &engine, &stats);

                    if let Some(warning) = &self.settings_warning {
                        ui.add_space(6.0);
                        warn_label(ui, warning);
                    }
                    if let Some(error) = &self.settings_save_error {
                        ui.add_space(6.0);
                        error_label(ui, error);
                    }
                });
        });

        self.draw_hotkey_capture(ui.ctx());
    }
}

/// Whether no run is active. The settings that a run uses change only while it is idle.
fn run_idle(engine: &State) -> bool {
    engine.run.phase == RunPhase::Idle
}

fn error_label(ui: &mut egui::Ui, text: impl Into<egui::RichText>) {
    ui.colored_label(ui.visuals().error_fg_color, text.into());
}

fn warn_label(ui: &mut egui::Ui, text: impl Into<egui::RichText>) {
    ui.colored_label(ui.visuals().warn_fg_color, text.into());
}

/// The one line for the MAKCU link: the phase, then the port and the firmware once it is connected.
/// The Devices panel and the diagnostics show it.
fn makcu_line(phase: OutputPhase, device: Option<&Device>) -> String {
    let phase = format!("{} {}", makcu_phase_mark(phase), makcu_phase_label(phase));
    match device {
        Some(device) => format!("{phase} · {} · FW {}", device.port, device.firmware_label()),
        None => phase,
    }
}

fn makcu_phase_label(phase: OutputPhase) -> &'static str {
    match phase {
        OutputPhase::Connecting => "Connecting",
        OutputPhase::Connected => "Connected",
        OutputPhase::Failed => "Disconnected",
    }
}

fn makcu_phase_mark(phase: OutputPhase) -> &'static str {
    match phase {
        OutputPhase::Connected => "●",
        OutputPhase::Connecting => "◌",
        OutputPhase::Failed => "○",
    }
}

/// The `XInput` hotkey pad: the slot, or that no pad is connected.
fn controller_line(slot: Option<u32>) -> String {
    match slot {
        Some(slot) => format!("● Connected · slot {slot}"),
        None => "○ Not connected".to_owned(),
    }
}

/// A percentage, as the panels show it.
fn percent(value: impl fmt::Display) -> String {
    format!("{value}%")
}
