use std::fs;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{models, paths};

pub const CONFIG_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    Mouse,
    Controller,
}

impl OutputMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mouse => "Mouse",
            Self::Controller => "Controller",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyAction {
    ContinuousHold,
    RunOnce,
}

impl HotkeyAction {
    pub const ALL: [Self; 2] = [Self::ContinuousHold, Self::RunOnce];

    pub fn label(self) -> &'static str {
        match self {
            Self::ContinuousHold => "Hold continuous",
            Self::RunOnce => "Run once",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyBinding {
    pub action: HotkeyAction,
    pub shortcut: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sensitivity(u16);

impl Sensitivity {
    pub const MIN: u16 = 1;
    pub const DEFAULT: u16 = 100;
    pub const MAX: u16 = 10_000;

    pub fn new(value: u16) -> Self {
        Self(value.clamp(Self::MIN, Self::MAX))
    }

    pub fn value(self) -> u16 {
        self.0
    }

    pub fn gain(self) -> f32 {
        self.0 as f32 / Self::DEFAULT as f32
    }

    pub fn adjusted(self, delta: i32) -> Self {
        let value = (i32::from(self.0) + delta).clamp(i32::from(Self::MIN), i32::from(Self::MAX));
        Self(value as u16)
    }
}

impl Default for Sensitivity {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub model: Option<String>,
    pub sensitivity: Sensitivity,
    pub output_mode: OutputMode,
    pub hotkeys: Vec<HotkeyBinding>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            model: None,
            sensitivity: Sensitivity::default(),
            output_mode: OutputMode::Mouse,
            hotkeys: default_hotkeys(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = paths::settings_path();
        let Ok(bytes) = fs::read(&path) else {
            return Self::default();
        };

        let version = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| value.get("version").and_then(|version| version.as_u64()));
        if version != Some(u64::from(CONFIG_VERSION)) {
            return Self::default();
        }

        let Ok(mut config) = serde_json::from_slice::<Self>(&bytes) else {
            return Self::default();
        };
        config.normalize();
        config
    }

    pub fn save(&self) -> Result<()> {
        let path = paths::settings_path();
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(&path, bytes).with_context(|| format!("failed to write {}", path.display()))
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

    fn normalize(&mut self) {
        self.version = CONFIG_VERSION;
        self.sensitivity = Sensitivity::new(self.sensitivity.value());
        self.hotkeys
            .retain(|binding| !binding.shortcut.trim().is_empty());

        if let Some(model) = &self.model
            && models::validate_model_name(model).is_err()
        {
            self.model = None;
        }
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
    fn sensitivity_is_integer_and_normalized() {
        assert_eq!(Sensitivity::new(0).value(), 1);
        assert_eq!(Sensitivity::default().gain(), 1.0);
        assert_eq!(Sensitivity::new(50).gain(), 0.5);
        assert_eq!(Sensitivity::new(200).gain(), 2.0);
        assert_eq!(Sensitivity::new(20_000).value(), 10_000);
    }

    #[test]
    fn bindings_can_be_removed_by_action_and_shortcut() {
        let mut config = Config::default();
        config.add_binding(HotkeyBinding {
            action: HotkeyAction::ContinuousHold,
            shortcut: "control+F8".to_owned(),
        });
        config.remove_binding(HotkeyAction::ContinuousHold, "F8");
        assert_eq!(config.bindings_for(HotkeyAction::ContinuousHold).count(), 1);
    }
}
