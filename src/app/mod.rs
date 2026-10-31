mod view;

use crate::{
    config::{Config, ConfigLoad},
    engine::{Command, Engine, EngineEvent},
    hotkeys::{HotkeyRegistry, canonical_shortcut},
    models::ModelCatalog,
    repaint::RepaintSignal,
    settings::{
        ConfidenceThreshold, FpsLimit, HotkeyAction, HotkeyBinding, Interpolation, MaxDelta,
        Sensitivity, TargetYOffset,
    },
};

pub struct App {
    config: Config,
    /// `gpu_debug` as applied at process start; the toggle only takes effect on restart.
    gpu_debug_at_startup: bool,
    engine: Engine,
    models: ModelCatalog,
    hotkeys: HotkeyRegistry,
    capturing_hotkey: Option<HotkeyAction>,
    hotkey_capture_armed: bool,
    hotkey_capture_error: Option<String>,
    hotkey_error: Option<String>,
    model_catalog_error: Option<String>,
    config_load_warning: Option<String>,
    /// The settings file could not be read at startup; the next save backs it up first.
    config_file_unusable: bool,
    config_save_error: Option<String>,
    dirty: UnsavedEdits,
    model_load_requested: bool,
}

/// Value fields edited since they were last saved (see `view::widgets::drag_value`).
#[derive(Default)]
struct UnsavedEdits {
    sensitivity: bool,
    confidence_threshold: bool,
    target_y_offset: bool,
    max_delta: bool,
    interpolation: bool,
    fps_limit: bool,
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
                tracing::error!(error = %crate::logging::chain(&error), "initial model scan failed");
                (ModelCatalog::default(), Some(format!("{error:#}")))
            }
        };

        let repaint = RepaintSignal::new(cc.egui_ctx.clone());
        let engine = Engine::new(&config, repaint.clone());
        let mut hotkeys = HotkeyRegistry::new(engine.control(), repaint);
        let hotkey_error = hotkeys.replace_bindings(&config.hotkeys).err();
        if let Some(error) = &hotkey_error {
            tracing::warn!(%error, "initial hotkey registration failed");
        }

        let app = Self {
            config,
            gpu_debug_at_startup,
            engine,
            models,
            hotkeys,
            capturing_hotkey: None,
            hotkey_capture_armed: false,
            hotkey_capture_error: None,
            hotkey_error,
            model_catalog_error,
            config_load_warning,
            config_file_unusable,
            config_save_error: None,
            dirty: UnsavedEdits::default(),
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
                tracing::error!(error = %crate::logging::chain(&error), "settings persistence failed");
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
                tracing::error!(error = %crate::logging::chain(&error), "model rescan failed");
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
        self.config.makcu_port = port.clone();
        self.engine.send(Command::SetMakcuPort(port));
        self.persist();
    }

    fn set_fps_limit(&mut self, fps_limit: FpsLimit) {
        tracing::debug!(value = fps_limit.value(), "FPS limit changed");
        self.config.fps_limit = fps_limit;
        self.engine.send(Command::SetFpsLimit(fps_limit));
    }

    fn begin_hotkey_capture(&mut self, action: HotkeyAction) {
        tracing::debug!(?action, "hotkey capture started");
        self.capturing_hotkey = Some(action);
        self.hotkey_capture_armed = false;
        self.hotkey_capture_error = None;
        self.hotkeys.begin_capture();
    }

    fn cancel_hotkey_capture(&mut self) {
        tracing::debug!("hotkey capture cancelled or completed");
        self.capturing_hotkey = None;
        self.hotkey_capture_armed = false;
        self.hotkey_capture_error = None;
        self.hotkeys.end_capture();
    }

    fn add_hotkey(&mut self, action: HotkeyAction, shortcut: String) {
        tracing::debug!(?action, shortcut = %shortcut, "adding hotkey binding");
        let shortcut = match canonical_shortcut(&shortcut) {
            Ok(shortcut) => shortcut,
            Err(error) => {
                tracing::warn!(%error, "hotkey shortcut was rejected");
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
            Err(error) => {
                tracing::warn!(%error, "hotkey registration failed");
                self.hotkey_capture_error = Some(error);
            }
        }
    }

    fn remove_hotkey(&mut self, action: HotkeyAction, shortcut: &str) {
        tracing::debug!(?action, shortcut, "removing hotkey binding");
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
            Err(error) => {
                tracing::warn!(%error, "hotkey removal failed");
                self.hotkey_error = Some(error);
            }
        }
    }
}
