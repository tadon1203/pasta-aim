mod view;

use std::{
    collections::HashSet,
    sync::{Arc, mpsc},
};

use nice_aimassist::{
    Message, Repaint, UiSender,
    config::{Config, ConfigLoad},
    engine::{Command, Engine, EngineEvent},
    hotkeys::{Action, Event, Hotkey, Hotkeys, Shortcut},
    models::ModelCatalog,
    settings::{
        ConfidenceThreshold, FpsLimit, Interpolation, MaxDelta, Sensitivity, TargetYOffset,
    },
};

pub struct App {
    config: Config,
    /// `gpu_debug` as applied at process start; the toggle only takes effect on restart.
    gpu_debug_at_startup: bool,
    engine: Engine,
    models: ModelCatalog,
    /// `None` when the input thread could not start; `hotkey_error` says why.
    hotkeys: Option<Hotkeys>,
    /// What the input thread reports to the UI.
    messages: mpsc::Receiver<Message>,
    /// The `XInput` controller in use, as the input thread last reported it.
    controller_slot: Option<u32>,
    /// The action of the binding that the user is adding. Some while the dialog is open.
    hotkey_capture: Option<Action>,
    hotkey_capture_error: Option<String>,
    hotkey_error: Option<String>,
    model_catalog_error: Option<String>,
    config_load_warning: Option<String>,
    /// The settings file could not be read at startup; the next save backs it up first.
    config_file_unusable: bool,
    config_save_error: Option<String>,
    /// Value fields edited since they were last saved (see `view::widgets::drag_value`).
    dirty: HashSet<EditField>,
    model_load_requested: bool,
}

/// A numeric setting edited through a widget that saves once the edit ends.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum EditField {
    Interpolation,
    Sensitivity,
    ConfidenceThreshold,
    TargetYOffset,
    MaxDelta,
    FpsLimit,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, loaded: ConfigLoad) -> Self {
        let config = loaded.config;
        let gpu_debug_at_startup = config.diagnostics.gpu_debug;
        let config_load_warning = loaded.warning;
        let config_file_unusable = loaded.file_unusable;
        tracing::info!(model = ?config.model, sensitivity = config.sensitivity.value(), fps_limit = config.fps_limit.value(), "application initialized with settings");
        let (models, model_catalog_error) = match ModelCatalog::scan() {
            Ok(catalog) => (catalog, None),
            Err(error) => {
                tracing::error!(error = %nice_aimassist::logging::chain(&error), "initial model scan failed");
                (ModelCatalog::default(), Some(format!("{error:#}")))
            }
        };

        let repaint: Repaint = {
            let ctx = cc.egui_ctx.clone();
            Arc::new(move || {
                if ctx.input(|input| input.viewport().minimized) != Some(true) {
                    ctx.request_repaint();
                }
            })
        };
        let engine = Engine::new(&config, Arc::clone(&repaint));

        let (tx, messages) = mpsc::channel();
        let ui = UiSender::new(tx, Arc::clone(&repaint));
        let control = engine.control();
        let on_action = move |action: Action, down: bool| {
            match action {
                Action::Hold => control.send(Command::SetContinuous(down)),
                Action::RunOnce if down => control.send(Command::RunOnce),
                Action::RunOnce => {}
            }
            repaint();
        };
        let (hotkeys, hotkey_error) = match Hotkeys::spawn(&config.hotkeys, ui, on_action) {
            Ok(hotkeys) => (Some(hotkeys), None),
            Err(error) => {
                tracing::error!(error = %nice_aimassist::logging::chain(&error), "hotkey input thread did not start");
                (None, Some(format!("{error:#}")))
            }
        };

        let app = Self {
            config,
            gpu_debug_at_startup,
            engine,
            models,
            hotkeys,
            messages,
            controller_slot: None,
            hotkey_capture: None,
            hotkey_capture_error: None,
            hotkey_error,
            model_catalog_error,
            config_load_warning,
            config_file_unusable,
            config_save_error: None,
            dirty: HashSet::new(),
            model_load_requested: false,
        };

        // The initial scan is the sole automatic-load source. Further scans
        // coalesce while this request is in flight.
        let mut app = app;
        app.request_sole_model_load();

        app
    }

    fn persist(&mut self) {
        match self.config.save(self.config_file_unusable) {
            Ok(()) => {
                tracing::debug!("settings persistence succeeded");
                self.config_save_error = None;
                self.config_load_warning = None;
                self.config_file_unusable = false;
            }
            Err(error) => {
                tracing::error!(error = %nice_aimassist::logging::chain(&error), "settings persistence failed");
                self.config_save_error = Some(format!("Settings save failed: {error:#}"));
            }
        }
    }

    fn process_engine_events(&mut self) {
        let events: Vec<_> = self.engine.poll_events().collect();
        for event in events {
            match event {
                EngineEvent::Model { name, result } => {
                    self.model_load_requested = false;
                    tracing::debug!(model = %name, success = result.is_ok(), "received model operation result");
                    if result.is_ok() {
                        self.config.model = Some(name);
                        self.persist();
                    }
                }
                EngineEvent::Capture {
                    adapter,
                    output,
                    adapter_luid,
                    result,
                } => {
                    tracing::debug!(
                        adapter,
                        output,
                        success = result.is_ok(),
                        "received capture operation result"
                    );
                    if result.is_ok() {
                        self.config.selected_adapter = adapter_luid;
                        self.config.capture_output = output;
                        self.persist();
                    }
                }
            }
        }
    }

    fn rescan_models(&mut self) {
        tracing::debug!("rescanning models from UI");
        match ModelCatalog::scan() {
            Ok(catalog) => {
                self.models = catalog;
                self.model_catalog_error = None;
                self.request_sole_model_load();
            }
            Err(error) => {
                tracing::error!(error = %nice_aimassist::logging::chain(&error), "model rescan failed");
                self.model_catalog_error = Some(format!("{error:#}"));
            }
        }
    }

    fn select_model(&mut self, name: String) {
        tracing::debug!(model = %name, "model selected from UI");
        if let Some(path) = self.models.path_for(&name) {
            self.engine.send(Command::LoadModel {
                name,
                path,
                notify: true,
            });
        }
    }

    fn request_sole_model_load(&mut self) {
        if self.config.model.is_none()
            && !self.model_load_requested
            && let Some(entry) = self.models.sole_model()
        {
            self.model_load_requested = true;
            self.engine.send(Command::LoadModel {
                name: entry.name.clone(),
                path: entry.path.clone(),
                notify: false,
            });
        }
    }

    fn rescan_displays(&self) {
        tracing::debug!("display rescan requested from UI");
        self.engine.send(Command::RescanDisplays);
    }

    fn select_display(&mut self, adapter: u32, output: u32) {
        tracing::debug!(adapter, output, "display selected from UI");
        self.engine
            .send(Command::SetCaptureSelection { adapter, output });
    }

    fn set_sensitivity(&mut self, sensitivity: Sensitivity) {
        tracing::debug!(value = sensitivity.value(), "sensitivity changed");
        self.config.sensitivity = sensitivity;
        self.engine.send(Command::SetSensitivity(sensitivity));
    }

    fn set_confidence_threshold(&mut self, value: ConfidenceThreshold) {
        tracing::debug!(value = value.value(), "confidence threshold changed");
        self.config.confidence_threshold = value;
        self.engine.send(Command::SetConfidenceThreshold(value));
    }

    fn set_target_y_offset(&mut self, value: TargetYOffset) {
        tracing::debug!(value = value.value(), "target Y offset changed");
        self.config.target_y_offset = value;
        self.engine.send(Command::SetTargetYOffset(value));
    }

    fn set_max_delta(&mut self, value: MaxDelta) {
        tracing::debug!(value = value.value(), "max delta changed");
        self.config.max_delta = value;
        self.engine.send(Command::SetMaxDelta(value));
    }

    /// Only stores and sends the value. The caller saves it with `persist`, after the drag or click ends.
    fn set_interpolation(&mut self, interpolation: Interpolation) {
        tracing::debug!(?interpolation, "move interpolation changed");
        self.config.interpolation = interpolation;
        self.engine.send(Command::SetInterpolation(interpolation));
    }

    /// `None` is auto-detect. The engine connects again, and the choice is saved.
    fn set_makcu_port(&mut self, port: Option<String>) {
        tracing::debug!(?port, "MAKCU port selected from UI");
        self.config.makcu_port.clone_from(&port);
        self.engine.send(Command::SetMakcuPort(port));
        self.persist();
    }

    fn set_fps_limit(&mut self, fps_limit: FpsLimit) {
        tracing::debug!(value = fps_limit.value(), "FPS limit changed");
        self.config.fps_limit = fps_limit;
        self.engine.send(Command::SetFpsLimit(fps_limit));
    }

    /// Applies what the input thread reported since the last frame.
    fn process_hotkey_messages(&mut self) {
        while let Ok(Message::Hotkeys(event)) = self.messages.try_recv() {
            match event {
                Event::Controller(slot) => self.controller_slot = slot,
                Event::Recorded(shortcut) => {
                    if let Some(action) = self.hotkey_capture {
                        self.add_hotkey(action, shortcut);
                    }
                }
                Event::RecordCancelled => {
                    self.hotkey_capture = None;
                    self.hotkey_capture_error = None;
                }
            }
        }
    }

    /// Replaces the bindings in the input thread. The error is text for the UI.
    fn apply_hotkeys(&self, hotkeys: &[Hotkey]) -> Result<(), String> {
        match &self.hotkeys {
            Some(running) => running.set(hotkeys).map_err(|error| format!("{error:#}")),
            None => Err("Hotkeys are not running".to_owned()),
        }
    }

    fn begin_hotkey_capture(&mut self, action: Action) {
        tracing::debug!(?action, "hotkey capture started");
        self.hotkey_capture = Some(action);
        self.hotkey_capture_error = None;
        if let Some(hotkeys) = &self.hotkeys {
            hotkeys.record();
        }
    }

    fn cancel_hotkey_capture(&mut self) {
        tracing::debug!("hotkey capture cancelled or completed");
        self.hotkey_capture = None;
        self.hotkey_capture_error = None;
        if let Some(hotkeys) = &self.hotkeys {
            hotkeys.cancel_record();
        }
    }

    fn add_hotkey(&mut self, action: Action, shortcut: Shortcut) {
        tracing::debug!(?action, %shortcut, "adding hotkey binding");
        let mut candidate = self.config.clone();
        candidate.add_binding(Hotkey { action, shortcut });
        match self.apply_hotkeys(&candidate.hotkeys) {
            Ok(()) => {
                self.config = candidate;
                self.hotkey_error = None;
                self.persist();
                self.cancel_hotkey_capture();
            }
            Err(error) => {
                // The recording has ended. Record again, so that the user can choose another shortcut.
                tracing::warn!(%error, "hotkey binding was rejected");
                self.hotkey_capture_error = Some(error);
                if let Some(hotkeys) = &self.hotkeys {
                    hotkeys.record();
                }
            }
        }
    }

    fn remove_hotkey(&mut self, action: Action, shortcut: Shortcut) {
        tracing::debug!(?action, %shortcut, "removing hotkey binding");
        let mut candidate = self.config.clone();
        candidate.remove_binding(action, shortcut);
        match self.apply_hotkeys(&candidate.hotkeys) {
            Ok(()) => {
                self.config = candidate;
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
