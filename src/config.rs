use std::fs;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    models, paths,
    settings::{
        ConfidenceThreshold, FpsLimit, HotkeyAction, HotkeyBinding, InjectionApi, MaxDelta,
        OverlayConfig, Sensitivity, TargetYOffset,
    },
};

/// Missing fields fall back to their defaults so that adding a setting never
/// discards the rest of an existing file; unknown fields are still rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub model: Option<String>,
    pub sensitivity: Sensitivity,
    pub confidence_threshold: ConfidenceThreshold,
    pub target_y_offset: TargetYOffset,
    /// Per-frame cap on the output movement length; 0 = unlimited.
    pub max_delta: MaxDelta,
    pub fps_limit: FpsLimit,
    pub injection_api: InjectionApi,
    pub hotkeys: Vec<HotkeyBinding>,
    pub selected_adapter: crate::inference::AdapterLuid,
    pub capture_output: u32,
    /// Busy-waits on DirectML completion for lower tail latency at the cost of one CPU core.
    pub low_latency: bool,
    pub overlay: OverlayConfig,
    pub diagnostics: DiagnosticsConfig,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
            injection_api: InjectionApi::default(),
            hotkeys: default_hotkeys(),
            selected_adapter: crate::inference::AdapterLuid { low: 0, high: 0 },
            capture_output: 0,
            low_latency: false,
            overlay: OverlayConfig::default(),
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
        if !self.max_delta.is_valid() {
            bail!("max_delta must be at most {}", MaxDelta::MAX);
        }
        if !self.fps_limit.is_valid() {
            bail!("fps_limit must be in {}..={}", FpsLimit::MIN, FpsLimit::MAX);
        }
        if !self.overlay.is_valid() {
            bail!("overlay settings are out of range");
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
        assert_eq!(config.injection_api, InjectionApi::MakApi);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults_and_keep_the_rest() {
        let config: Config =
            serde_json::from_str(r#"{"sensitivity": 250, "low_latency": true}"#).unwrap();
        assert_eq!(config.sensitivity.value(), 250);
        assert!(config.low_latency);
        assert_eq!(config.confidence_threshold, ConfidenceThreshold::default());
        assert_eq!(config.hotkeys.len(), default_hotkeys().len());
        assert_eq!(config.overlay, OverlayConfig::default());
    }

    #[test]
    fn overlay_accepts_partial_settings_and_validates_ranges() {
        let config: Config =
            serde_json::from_str(r#"{"overlay": {"enabled": true, "line_width": 4}}"#).unwrap();
        assert!(config.overlay.enabled);
        assert_eq!(config.overlay.line_width.value(), 4);
        assert_eq!(
            config.overlay.outline_width,
            OverlayConfig::default().outline_width
        );
        assert!(config.validate().is_ok());

        let out_of_range: Config =
            serde_json::from_str(r#"{"overlay": {"line_width": 200}}"#).unwrap();
        assert!(out_of_range.validate().is_err());
    }

    #[test]
    fn version_field_is_not_recognized() {
        assert!(serde_json::from_str::<Config>(r#"{"version": 2}"#).is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let json = serde_json::to_string(&Config::default()).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value["unexpected"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<Config>(value).is_err());
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
}
