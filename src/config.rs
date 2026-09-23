use std::fs;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    models, paths,
    settings::{
        DirectMlDevice, FpsLimit, HotkeyAction, HotkeyBinding, InjectionApi, OutputMode,
        Sensitivity,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub model: Option<String>,
    pub sensitivity: Sensitivity,
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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsConfig {
    pub frame_trace: bool,
    pub mak_command_trace: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: None,
            sensitivity: Sensitivity::default(),
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
                tracing::info!(path = %path.display(), "settings file not found; using defaults");
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
}
