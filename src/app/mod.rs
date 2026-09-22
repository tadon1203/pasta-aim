mod view;

use crate::{
    config::Config,
    engine::{Engine, EngineEvent},
    hotkeys::{HotkeyRegistry, canonical_shortcut},
    models::ModelCatalog,
    settings::{DirectMlDevice, FpsLimit, HotkeyAction, HotkeyBinding, Sensitivity},
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
    config_load_warning: Option<String>,
    config_save_error: Option<String>,
    sensitivity_dirty: bool,
    fps_limit_dirty: bool,
    model_load_requested: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let loaded = Config::load();
        let config = loaded.config;
        crate::logging::set_diagnostics(config.diagnostics);
        let config_load_warning = loaded.warning;
        tracing::info!(model = ?config.model, sensitivity = config.sensitivity.value(), fps_limit = config.fps_limit.value(), output_mode = ?config.output_mode, "application initialized with settings");
        let (models, model_catalog_error) = match ModelCatalog::scan() {
            Ok(catalog) => (catalog, None),
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "initial model scan failed");
                (ModelCatalog::default(), Some(format!("{error:#}")))
            }
        };

        let engine = Engine::new(&config, cc.egui_ctx.clone());
        let mut hotkeys = HotkeyRegistry::new(engine.control(), cc.egui_ctx.clone());
        let hotkey_error = hotkeys.replace_bindings(&config.hotkeys).err();
        if let Some(error) = &hotkey_error {
            tracing::warn!(%error, "initial hotkey registration failed");
        }

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
            config_load_warning,
            config_save_error: None,
            sensitivity_dirty: false,
            fps_limit_dirty: false,
            model_load_requested: false,
        };

        // The initial scan is the sole automatic-load source. Further scans
        // coalesce while this request is in flight.
        let mut app = app;
        app.request_sole_model_load();

        app
    }

    fn persist(&mut self) {
        match self.config.save() {
            Ok(()) => {
                tracing::debug!("settings persistence succeeded");
                self.config_save_error = None;
                self.config_load_warning = None;
            }
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "settings persistence failed");
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
                    result,
                } => {
                    tracing::debug!(
                        adapter,
                        output,
                        success = result.is_ok(),
                        "received capture operation result"
                    );
                    if result.is_ok() {
                        self.config.capture_adapter = adapter;
                        self.config.capture_output = output;
                        self.persist();
                    }
                }
                EngineEvent::DirectMl { device, result } => {
                    tracing::debug!(
                        ?device,
                        success = result.is_ok(),
                        "received DirectML operation result"
                    );
                    if result.is_ok() {
                        self.config.directml_device = device;
                        self.persist();
                    }
                }
            }
        }
    }

    fn rescan_models(&mut self) {
        tracing::info!("rescanning models from UI");
        match ModelCatalog::scan() {
            Ok(catalog) => {
                self.models = catalog;
                self.model_catalog_error = None;
                self.request_sole_model_load();
            }
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "model rescan failed");
                self.model_catalog_error = Some(format!("{error:#}"));
            }
        }
    }

    fn select_model(&mut self, name: String) {
        tracing::info!(model = %name, "model selected from UI");
        if let Some(path) = self.models.path_for(&name) {
            self.engine.load_model(name, path);
        }
    }

    fn request_sole_model_load(&mut self) {
        if self.config.model.is_none()
            && !self.model_load_requested
            && let Some(entry) = self.models.sole_model()
        {
            self.model_load_requested = true;
            self.engine
                .load_model_transient(entry.name.clone(), entry.path.clone());
        }
    }

    fn rescan_displays(&self) {
        tracing::info!("display rescan requested from UI");
        self.engine.rescan_displays();
    }

    fn select_display(&mut self, adapter: u32, output: u32) {
        tracing::info!(adapter, output, "display selected from UI");
        self.engine.set_capture_selection(adapter, output);
    }

    fn select_directml_device(&mut self, device: DirectMlDevice) {
        tracing::info!(?device, "DirectML device selected from UI");
        self.engine.set_directml_device(device);
    }

    fn set_sensitivity(&mut self, sensitivity: Sensitivity, persist: bool) {
        tracing::debug!(value = sensitivity.value(), persist, "sensitivity changed");
        self.config.sensitivity = sensitivity;
        self.engine.set_sensitivity(sensitivity);
        if persist {
            self.persist();
        }
    }

    fn set_fps_limit(&mut self, fps_limit: FpsLimit, persist: bool) {
        tracing::debug!(value = fps_limit.value(), persist, "FPS limit changed");
        self.config.fps_limit = fps_limit;
        self.engine.set_fps_limit(fps_limit);
        if persist {
            self.persist();
        }
    }

    fn begin_hotkey_capture(&mut self, action: HotkeyAction) {
        tracing::info!(?action, "hotkey capture started");
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
        tracing::info!(?action, shortcut = %shortcut, "adding hotkey binding");
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
        tracing::info!(?action, shortcut, "removing hotkey binding");
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
