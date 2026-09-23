use std::fs;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    models, paths,
    settings::{
        ConfidenceThreshold, DirectMlDevice, FpsLimit, HotkeyAction, HotkeyBinding, InjectionApi,
        OutputMode, Sensitivity, TargetYOffset,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub model: Option<String>,
    pub sensitivity: Sensitivity,
    pub confidence_threshold: ConfidenceThreshold,
    pub target_y_offset: TargetYOffset,
    pub fps_limit: FpsLimit,
    pub output_mode: OutputMode,
    #[serde(default)]
    pub injection_api: InjectionApi,
    pub hotkeys: Vec<HotkeyBinding>,
    pub capture_adapter: u32,
    pub capture_output: u32,
    pub directml_device: DirectMlDevice,
    #[serde(default)]
    pub diagnostics: DiagnosticsConfig,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsConfig {
    pub level: LogLevel,
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

impl<'de> Deserialize<'de> for DiagnosticsConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawDiagnostics {
            level: Option<LogLevel>,
            frame_trace: Option<bool>,
            mak_command_trace: Option<bool>,
        }

        let raw = RawDiagnostics::deserialize(deserializer)?;
        let level = raw.level.unwrap_or_else(|| {
            if raw.frame_trace.unwrap_or(false) || raw.mak_command_trace.unwrap_or(false) {
                LogLevel::Trace
            } else {
                LogLevel::Info
            }
        });
        Ok(Self { level })
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: None,
            sensitivity: Sensitivity::default(),
            confidence_threshold: ConfidenceThreshold::default(),
            target_y_offset: TargetYOffset::default(),
            fps_limit: FpsLimit::default(),
            output_mode: OutputMode::Mouse,
            injection_api: InjectionApi::default(),
            hotkeys: default_hotkeys(),
            capture_adapter: 0,
            capture_output: 0,
            directml_device: DirectMlDevice::default(),
            diagnostics: DiagnosticsConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigLoad {
    pub config: Config,
    pub warning: Option<String>,
}

impl Config {
    pub fn load() -> ConfigLoad {
        let path = paths::settings_path();
        tracing::debug!(path = %path.display(), "loading settings");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "settings file not found; using defaults");
                return ConfigLoad {
                    config: Self::default(),
                    warning: None,
                };
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "settings file could not be read; using defaults");
                return ConfigLoad {
                    config: Self::default(),
                    warning: Some(format!("Settings could not be read: {error}")),
                };
            }
        };

        match serde_json::from_slice::<Self>(&bytes)
            .map_err(anyhow::Error::from)
            .and_then(|config| {
                config.validate()?;
                Ok(config)
            }) {
            Ok(config) => ConfigLoad {
                config,
                warning: None,
            },
            Err(error) => {
                tracing::warn!(path = %path.display(), error = %format!("{error:#}"), "settings file could not be parsed; using defaults");
                ConfigLoad {
                    config: Self::default(),
                    warning: Some(format!(
                        "Settings could not be loaded. Using defaults until the next settings change: {error:#}"
                    )),
                }
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        self.validate()?;
        let path = paths::settings_path();
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
        if !self.fps_limit.is_valid() {
            bail!("fps_limit must be in {}..={}", FpsLimit::MIN, FpsLimit::MAX);
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
        if let DirectMlDevice::Device(index) = self.directml_device
            && index < 0
        {
            bail!("DirectML device index must be non-negative");
        }
        Ok(())
    }
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
    fn current_config_deserializes_keyboard_and_xinput_bindings() {
        let json = r#"{
            "model": null,
            "sensitivity": 100,
            "confidence_threshold": 50,
            "target_y_offset": 0,
            "fps_limit": 240,
            "output_mode": "mouse",
            "hotkeys": [
                {"action": "continuous_hold", "shortcut": "F8"},
                {"action": "run_once", "shortcut": "xinput:a"}
            ],
            "capture_adapter": 0,
            "capture_output": 0,
            "directml_device": "auto_high_performance"
        }"#;

        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.hotkeys.len(), 2);
        assert_eq!(config.hotkeys[0].shortcut, "F8");
        assert_eq!(config.hotkeys[1].shortcut, "xinput:a");
        assert_eq!(config.injection_api, InjectionApi::MakApi);
    }

    #[test]
    fn confidence_threshold_is_required_in_the_current_config_format() {
        let json = r#"{
            "model": null,
            "sensitivity": 100,
            "target_y_offset": 0,
            "fps_limit": 240,
            "output_mode": "mouse",
            "hotkeys": [],
            "capture_adapter": 0,
            "capture_output": 0,
            "directml_device": "auto_high_performance"
        }"#;

        assert!(serde_json::from_str::<Config>(json).is_err());
    }

    #[test]
    fn missing_required_fields_are_rejected() {
        let incomplete = r#"{"model":null,"sensitivity":100,"output_mode":"mouse","hotkeys":[]}"#;
        assert!(serde_json::from_str::<Config>(incomplete).is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let json = serde_json::to_string(&Config::default()).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value["unexpected"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<Config>(value).is_err());
    }

    #[test]
    fn target_y_offset_is_required() {
        let json = r#"{
            "model": null,
            "sensitivity": 100,
            "fps_limit": 240,
            "output_mode": "mouse",
            "hotkeys": [],
            "capture_adapter": 0,
            "capture_output": 0,
            "directml_device": "auto_high_performance"
        }"#;

        assert!(serde_json::from_str::<Config>(json).is_err());
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
    fn legacy_trace_flags_are_migrated_to_trace_level() {
        let json = r#"{
            "level": null,
            "frame_trace": true,
            "mak_command_trace": false
        }"#;

        let diagnostics: DiagnosticsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(diagnostics.level, LogLevel::Trace);
    }

    #[test]
    fn explicit_level_takes_precedence_over_legacy_trace_flags() {
        let json = r#"{
            "level": "warn",
            "frame_trace": true,
            "mak_command_trace": true
        }"#;

        let diagnostics: DiagnosticsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(diagnostics.level, LogLevel::Warn);
    }
}
