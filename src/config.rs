use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    models, paths,
    settings::{
        CaptureSettings, ConfidenceThreshold, FpsLimit, HotkeyAction, HotkeyBinding, Interpolation,
        InterpolationPercent, MaxDelta, Sensitivity, TargetYOffset,
    },
};

/// Missing fields fall back to their defaults so that adding a setting never
/// discards the rest of an existing file. Unknown fields are ignored here and
/// removed by `Config::load`, which also resets each invalid field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub model: Option<String>,
    pub sensitivity: Sensitivity,
    pub confidence_threshold: ConfidenceThreshold,
    pub target_y_offset: TargetYOffset,
    /// Per-frame cap on the output movement length; 0 = unlimited.
    pub max_delta: MaxDelta,
    pub fps_limit: FpsLimit,
    pub capture: CaptureSettings,
    pub hotkeys: Vec<HotkeyBinding>,
    pub selected_adapter: crate::inference::AdapterLuid,
    pub capture_output: u32,
    /// Busy-waits on `DirectML` completion for lower tail latency at the cost of one CPU core.
    pub low_latency: bool,
    pub interpolation: Interpolation,
    /// The serial port of MAKCU, such as `COM4`. `None` detects it automatically.
    pub makcu_port: Option<String>,
    pub diagnostics: DiagnosticsConfig,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosticsConfig {
    pub level: LogLevel,
    /// D3D12 debug layer + DRED. Applied at startup only; slows inference.
    pub gpu_debug: bool,
    /// Writes a per-frame JSON Lines trace to `logs/` for each run.
    pub frame_trace: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: None,
            sensitivity: Sensitivity::default(),
            confidence_threshold: ConfidenceThreshold::default(),
            target_y_offset: TargetYOffset::default(),
            max_delta: MaxDelta::default(),
            fps_limit: FpsLimit::default(),
            capture: CaptureSettings::default(),
            hotkeys: default_hotkeys(),
            selected_adapter: crate::inference::AdapterLuid { low: 0, high: 0 },
            capture_output: 0,
            low_latency: false,
            interpolation: Interpolation::default(),
            makcu_port: None,
            diagnostics: DiagnosticsConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigLoad {
    pub config: Config,
    pub warning: Option<String>,
    /// The file exists but could not be read or parsed. The next save backs it up first.
    pub file_unusable: bool,
}

impl Config {
    pub fn load() -> ConfigLoad {
        let path = paths::settings_path();
        tracing::debug!(path = %path.display(), "loading settings");
        match fs::read(&path) {
            Ok(bytes) => Self::parse(&bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "settings file not found; using defaults");
                ConfigLoad {
                    config: Self::default(),
                    warning: None,
                    file_unusable: false,
                }
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "settings file could not be read; using defaults");
                ConfigLoad {
                    config: Self::default(),
                    warning: Some(format!("Settings could not be read: {error}")),
                    file_unusable: true,
                }
            }
        }
    }

    /// Reads a settings file. Invalid JSON, or JSON that is not an object, uses the defaults.
    fn parse(bytes: &[u8]) -> ConfigLoad {
        match Self::read_fields(bytes) {
            Ok(loaded) => loaded,
            Err(error) => {
                tracing::warn!(error = %crate::logging::chain(&error), "settings file could not be parsed; using defaults");
                ConfigLoad {
                    config: Self::default(),
                    warning: Some(format!(
                        "Settings could not be loaded. Using defaults until the next settings change: {error:#}"
                    )),
                    file_unusable: true,
                }
            }
        }
    }

    fn read_fields(bytes: &[u8]) -> Result<ConfigLoad> {
        let Value::Object(fields) = serde_json::from_slice::<Value>(bytes)? else {
            bail!("settings must be a JSON object");
        };
        let mut loader = FieldLoader::new();
        loader.load_object(&fields, "", "");
        Ok(ConfigLoad {
            warning: loader.warning(),
            config: loader.config,
            file_unusable: false,
        })
    }

    /// Writes the settings. When the file was unusable at load, it is copied to
    /// `settings.json.bak` first, so that the save does not lose its content.
    pub fn save(&self, file_unusable: bool) -> Result<()> {
        self.validate()?;
        let path = paths::settings_path();
        if file_unusable && path.exists() {
            back_up(&path)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(&path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
        tracing::debug!(path = %path.display(), "settings saved");
        Ok(())
    }

    pub fn bindings_for(&self, action: HotkeyAction) -> impl Iterator<Item = &HotkeyBinding> {
        self.hotkeys
            .iter()
            .filter(move |binding| binding.action == action)
    }

    pub fn add_binding(&mut self, binding: HotkeyBinding) {
        self.hotkeys.push(binding);
    }

    pub fn remove_binding(&mut self, action: HotkeyAction, shortcut: &str) {
        self.hotkeys
            .retain(|binding| !(binding.action == action && binding.shortcut == shortcut));
    }

    fn validate(&self) -> Result<()> {
        if !self.sensitivity.is_valid() {
            bail!(
                "sensitivity must be in {}..={}",
                Sensitivity::MIN,
                Sensitivity::MAX
            );
        }
        if !self.confidence_threshold.is_valid() {
            bail!(
                "confidence_threshold must be in {}..={}",
                ConfidenceThreshold::MIN,
                ConfidenceThreshold::MAX
            );
        }
        if !self.target_y_offset.is_valid() {
            bail!(
                "target_y_offset must be in {}..={}",
                TargetYOffset::MIN,
                TargetYOffset::MAX
            );
        }
        if !self.max_delta.is_valid() {
            bail!("max_delta must be at most {}", MaxDelta::MAX);
        }
        if !self.fps_limit.is_valid() {
            bail!("fps_limit must be in {}..={}", FpsLimit::MIN, FpsLimit::MAX);
        }
        if !self.capture.is_valid() {
            bail!("capture settings are out of range");
        }
        if let Interpolation::Percent(percent) = self.interpolation
            && !percent.is_valid()
        {
            bail!(
                "interpolation must be in {}..={} percent",
                InterpolationPercent::MIN,
                InterpolationPercent::MAX
            );
        }
        if self
            .makcu_port
            .as_ref()
            .is_some_and(|port| port.trim().is_empty())
        {
            bail!("makcu_port cannot be empty");
        }

        if let Some(model) = &self.model {
            models::validate_model_name(model)?;
        }
        if self
            .hotkeys
            .iter()
            .any(|binding| binding.shortcut.trim().is_empty())
        {
            bail!("hotkey shortcut cannot be empty");
        }
        Ok(())
    }
}

/// Loads a settings file one field at a time. Each change is checked against the
/// whole config, so one invalid field does not reject the fields around it.
struct FieldLoader {
    /// The settings so far: the defaults, with each accepted field applied.
    fields: Value,
    config: Config,
    removed: Vec<String>,
    reset: Vec<String>,
}

impl FieldLoader {
    fn new() -> Self {
        let config = Config::default();
        Self {
            fields: serde_json::to_value(&config).expect("the default config serializes"),
            config,
            removed: Vec::new(),
            reset: Vec::new(),
        }
    }

    fn load_object(&mut self, file: &Map<String, Value>, pointer: &str, name: &str) {
        for (key, value) in file {
            let child = child_pointer(pointer, key);
            if self.fields.pointer(&child).is_none() {
                self.removed.push(join_name(name, key));
            } else {
                self.load_value(value, &child, &join_name(name, key));
            }
        }
    }

    fn load_value(&mut self, file: &Value, pointer: &str, name: &str) {
        let current = self.fields.pointer(pointer).cloned();
        match (file, current) {
            (Value::Object(fields), Some(Value::Object(_))) => {
                self.load_object(fields, pointer, name);
            }
            (Value::Array(items), Some(Value::Array(_))) => self.load_array(items, pointer, name),
            _ => {
                if !self.accept(pointer, file.clone()) {
                    self.reset.push(name.to_owned());
                }
            }
        }
    }

    /// Keeps each element that is valid on its own. An element is removed when it is
    /// invalid or when it is not an object. Unknown keys inside an element are removed.
    fn load_array(&mut self, items: &[Value], pointer: &str, name: &str) {
        let template = self
            .fields
            .pointer(pointer)
            .and_then(Value::as_array)
            .and_then(|list| list.first())
            .and_then(Value::as_object)
            .cloned();
        if let Some(slot) = self.fields.pointer_mut(pointer) {
            *slot = Value::Array(Vec::new());
        }
        for (index, item) in items.iter().enumerate() {
            let item_name = format!("{name}[{index}]");
            if let (Value::Object(fields), Some(template)) = (item, &template) {
                for key in fields.keys().filter(|key| !template.contains_key(*key)) {
                    self.removed.push(join_name(&item_name, key));
                }
            }
            let mut list = self
                .fields
                .pointer(pointer)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            list.push(item.clone());
            if !self.accept(pointer, Value::Array(list)) {
                self.removed.push(item_name);
            }
        }
    }

    /// Stores `value` at `pointer` when the settings stay valid with it.
    fn accept(&mut self, pointer: &str, value: Value) -> bool {
        let mut fields = self.fields.clone();
        let Some(slot) = fields.pointer_mut(pointer) else {
            return false;
        };
        *slot = value;
        let Ok(config) = serde_json::from_value::<Config>(fields.clone()) else {
            return false;
        };
        if config.validate().is_err() {
            return false;
        }
        self.fields = fields;
        self.config = config;
        true
    }

    fn warning(&self) -> Option<String> {
        let mut parts = Vec::new();
        if !self.removed.is_empty() {
            parts.push(format!("removed {}", self.removed.join(", ")));
        }
        if !self.reset.is_empty() {
            parts.push(format!("reset to default {}", self.reset.join(", ")));
        }
        (!parts.is_empty()).then(|| format!("Settings: {}", parts.join("; ")))
    }
}

/// The name of a field in a message, such as `capture.wait_ms`.
fn join_name(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_owned()
    } else {
        format!("{parent}.{key}")
    }
}

/// The JSON pointer of `key` under `pointer`. It escapes `~` and `/` as the pointer syntax requires.
fn child_pointer(pointer: &str, key: &str) -> String {
    format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1"))
}

fn back_up(path: &Path) -> Result<()> {
    let mut backup = path.as_os_str().to_owned();
    backup.push(".bak");
    fs::copy(path, &backup).with_context(|| format!("failed to back up {}", path.display()))?;
    Ok(())
}

fn default_hotkeys() -> Vec<HotkeyBinding> {
    vec![
        HotkeyBinding {
            action: HotkeyAction::ContinuousHold,
            shortcut: "F8".to_owned(),
        },
        HotkeyBinding {
            action: HotkeyAction::RunOnce,
            shortcut: "F9".to_owned(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_policy_defaults_round_trips_and_validates_ranges() {
        let old: Config = serde_json::from_str(r#"{"fps_limit":120}"#).unwrap();
        assert_eq!(old.capture, CaptureSettings::default());
        assert_eq!(old.fps_limit.value(), 120);
        let mut config: Config = serde_json::from_str(r#"{"capture":{"wait_ms":1}}"#).unwrap();
        config.validate().unwrap();
        let restored: Config =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(restored.capture, config.capture);
        for invalid in [0, 21] {
            config.capture.wait_ms = invalid;
            assert!(config.validate().is_err());
        }
    }

    #[test]
    fn current_config_deserializes_keyboard_and_xinput_bindings() {
        let json = r#"{
            "model": null,
            "sensitivity": 100,
            "confidence_threshold": 50,
            "target_y_offset": 0,
            "fps_limit": 240,
            "hotkeys": [
                {"action": "continuous_hold", "shortcut": "F8"},
                {"action": "run_once", "shortcut": "xinput:a"}
            ],
            "selected_adapter": {"low": 0, "high": 0},
            "capture_output": 0
        }"#;

        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.hotkeys.len(), 2);
        assert_eq!(config.hotkeys[0].shortcut, "F8");
        assert_eq!(config.hotkeys[1].shortcut, "xinput:a");
    }

    #[test]
    fn old_injection_api_key_is_removed_and_the_rest_is_kept() {
        let loaded = Config::parse(br#"{"injection_api": "km_api", "fps_limit": 120}"#);
        assert_eq!(loaded.config.fps_limit.value(), 120);
        assert!(!loaded.file_unusable);
        let warning = loaded.warning.unwrap();
        assert!(warning.contains("injection_api"));
    }

    #[test]
    fn max_delta_defaults_to_512() {
        assert_eq!(Config::default().max_delta.value(), 512);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults_and_keep_the_rest() {
        let config: Config =
            serde_json::from_str(r#"{"sensitivity": 250, "low_latency": true}"#).unwrap();
        assert_eq!(config.sensitivity.value(), 250);
        assert!(config.low_latency);
        assert_eq!(config.confidence_threshold, ConfidenceThreshold::default());
        assert_eq!(config.hotkeys.len(), default_hotkeys().len());
    }

    #[test]
    fn unknown_fields_are_removed_and_reported_and_the_rest_is_kept() {
        let loaded = Config::parse(
            br#"{"sensitivity": 150, "version": 2, "capture": {"wait_ms": 3, "extra": true}}"#,
        );
        assert_eq!(loaded.config.sensitivity.value(), 150);
        assert_eq!(loaded.config.capture.wait_ms, 3);
        assert!(!loaded.file_unusable);
        let warning = loaded.warning.unwrap();
        assert!(warning.contains("version"));
        assert!(warning.contains("capture.extra"));
    }

    #[test]
    fn invalid_values_are_reset_to_their_defaults_and_the_rest_is_kept() {
        let loaded = Config::parse(
            br#"{"sensitivity": 0, "target_y_offset": "top", "fps_limit": 120, "capture": {"wait_ms": 0}}"#,
        );
        assert_eq!(loaded.config.sensitivity, Sensitivity::default());
        assert_eq!(loaded.config.target_y_offset, TargetYOffset::default());
        assert_eq!(loaded.config.fps_limit.value(), 120);
        assert_eq!(
            loaded.config.capture.wait_ms,
            CaptureSettings::default().wait_ms
        );
        let warning = loaded.warning.unwrap();
        assert!(warning.contains("sensitivity"));
        assert!(warning.contains("target_y_offset"));
        assert!(warning.contains("capture.wait_ms"));
    }

    #[test]
    fn invalid_hotkey_entry_is_removed_and_the_other_entries_are_kept() {
        let loaded = Config::parse(
            br#"{"hotkeys": [
                {"action": "run_once", "shortcut": "F9"},
                {"action": "run_once", "shortcut": " "},
                {"action": "continuous_hold", "shortcut": "F8", "extra": 1}
            ]}"#,
        );
        assert_eq!(loaded.config.hotkeys.len(), 2);
        assert_eq!(loaded.config.hotkeys[0].shortcut, "F9");
        assert_eq!(loaded.config.hotkeys[1].shortcut, "F8");
        let warning = loaded.warning.unwrap();
        assert!(warning.contains("hotkeys[1]"));
        assert!(warning.contains("hotkeys[2].extra"));
    }

    #[test]
    fn unreadable_file_uses_defaults_and_is_marked_for_backup() {
        let broken = Config::parse(br#"{"sensitivity": 150"#);
        assert_eq!(broken.config.sensitivity, Sensitivity::default());
        assert!(broken.file_unusable);
        assert!(broken.warning.is_some());
        assert!(Config::parse(b"[1, 2]").file_unusable);
    }

    #[test]
    fn valid_file_has_no_warning_and_is_not_marked_for_backup() {
        let loaded = Config::parse(br#"{"fps_limit": 120}"#);
        assert!(loaded.warning.is_none());
        assert!(!loaded.file_unusable);
    }

    #[test]
    fn target_y_offset_range_is_validated() {
        let config = Config {
            target_y_offset: serde_json::from_value(serde_json::json!(101)).unwrap(),
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn diagnostics_default_to_info_and_round_trip_level() {
        let config = Config::default();
        assert_eq!(config.diagnostics.level, LogLevel::Info);

        let json = serde_json::to_string(&config).unwrap();
        let restored: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.diagnostics.level, LogLevel::Info);
    }

    #[test]
    fn gpu_debug_defaults_off_and_is_independent_of_level() {
        let diagnostics: DiagnosticsConfig = serde_json::from_str(r#"{"level": "trace"}"#).unwrap();
        assert_eq!(diagnostics.level, LogLevel::Trace);
        assert!(!diagnostics.gpu_debug);

        let diagnostics = DiagnosticsConfig {
            level: LogLevel::Warn,
            gpu_debug: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&diagnostics).unwrap();
        let restored: DiagnosticsConfig = serde_json::from_str(&json).unwrap();
        assert!(restored.gpu_debug);
        assert_eq!(restored.level, LogLevel::Warn);
    }

    #[test]
    fn low_latency_defaults_off_and_round_trips() {
        assert!(!Config::default().low_latency);
        let config = Config {
            low_latency: true,
            ..Config::default()
        };
        let restored: Config =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert!(restored.low_latency);
    }

    #[test]
    fn the_makcu_port_defaults_to_auto_and_round_trips() {
        assert_eq!(Config::default().makcu_port, None);
        let config = Config {
            makcu_port: Some("COM4".to_owned()),
            ..Config::default()
        };
        let restored = Config::parse(&serde_json::to_vec(&config).unwrap());
        assert_eq!(restored.config.makcu_port.as_deref(), Some("COM4"));
        assert!(restored.warning.is_none());
    }

    #[test]
    fn an_empty_makcu_port_falls_back_to_auto_and_the_rest_is_kept() {
        let loaded = Config::parse(br#"{"makcu_port": " ", "fps_limit": 120}"#);
        assert_eq!(loaded.config.makcu_port, None);
        assert_eq!(loaded.config.fps_limit.value(), 120);
        assert!(loaded.warning.unwrap().contains("makcu_port"));
    }

    #[test]
    fn interpolation_defaults_to_auto() {
        let config = Config::default();
        assert_eq!(config.interpolation, Interpolation::Auto);
    }

    #[test]
    fn an_out_of_range_interpolation_falls_back_to_auto_and_the_rest_is_kept() {
        let loaded = Config::parse(br#"{"interpolation": {"percent": 150}, "fps_limit": 120}"#);
        assert_eq!(loaded.config.interpolation, Interpolation::Auto);
        assert_eq!(loaded.config.fps_limit.value(), 120);
        assert!(loaded.warning.unwrap().contains("interpolation"));
    }

    #[test]
    fn an_interpolation_percent_round_trips_through_the_file() {
        let config = Config {
            interpolation: Interpolation::Percent(InterpolationPercent::new(40)),
            ..Config::default()
        };
        let restored = Config::parse(&serde_json::to_vec(&config).unwrap());
        assert_eq!(restored.config.interpolation, config.interpolation);
        assert!(restored.warning.is_none());
    }
}
