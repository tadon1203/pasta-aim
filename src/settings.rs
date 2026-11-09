use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::hotkeys::{Action, Hotkey, Shortcut};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "u16", into = "u16")]
pub struct Ranged<const MIN: u16, const DEFAULT: u16, const MAX: u16>(u16);

impl<const MIN: u16, const DEFAULT: u16, const MAX: u16> Ranged<MIN, DEFAULT, MAX> {
    pub const MIN: u16 = MIN;
    pub const DEFAULT: u16 = DEFAULT;
    pub const MAX: u16 = MAX;

    pub fn new(value: u16) -> Self {
        Self(value.clamp(MIN, MAX))
    }

    pub fn get(self) -> u16 {
        self.0
    }
}

impl<const MIN: u16, const DEFAULT: u16, const MAX: u16> Default for Ranged<MIN, DEFAULT, MAX> {
    fn default() -> Self {
        Self(DEFAULT)
    }
}

impl<const MIN: u16, const DEFAULT: u16, const MAX: u16> From<u16> for Ranged<MIN, DEFAULT, MAX> {
    fn from(value: u16) -> Self {
        Self::new(value)
    }
}

impl<const MIN: u16, const DEFAULT: u16, const MAX: u16> From<Ranged<MIN, DEFAULT, MAX>> for u16 {
    fn from(value: Ranged<MIN, DEFAULT, MAX>) -> Self {
        value.0
    }
}

pub type Sensitivity = Ranged<1, 100, 10_000>;
pub type Confidence = Ranged<0, 50, 100>;
pub type TargetYOffset = Ranged<0, 50, 100>;
pub type MaxDelta = Ranged<0, 512, 32_767>;
pub type VelocitySmoothing = Ranged<0, 50, 95>;
pub type FpsLimit = Ranged<1, 240, 1_000>;
pub type NewFrameWait = Ranged<1, 2, 20>;
pub type InterpolationPercent = Ranged<0, 0, 100>;

impl Sensitivity {
    pub fn gain(self) -> f32 {
        f32::from(self.get()) / f32::from(Self::DEFAULT)
    }
}

impl VelocitySmoothing {
    pub fn blend(self) -> f32 {
        1.0 - f32::from(self.get()) / 100.0
    }
}

impl Confidence {
    pub fn ratio(self) -> f32 {
        f32::from(self.get()) / f32::from(Self::MAX)
    }
}

impl TargetYOffset {
    pub fn center_relative(self) -> f32 {
        (f32::from(self.get()) - f32::from(Self::DEFAULT)) / 100.0
    }
}

impl MaxDelta {
    pub fn limit(self) -> Option<f32> {
        (self.get() != Self::MIN).then(|| f32::from(self.get()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Interpolation {
    #[default]
    Auto,
    Percent(InterpolationPercent),
}

impl Interpolation {
    pub fn wire_value(self) -> u8 {
        match self {
            Self::Auto => 255,
            // The percent is kept in 0..=100, so it fits a byte.
            Self::Percent(percent) => percent.get() as u8,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    pub adapter: u64,
    pub output: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub model: Option<String>,
    pub display: Display,
    pub sensitivity_x: Sensitivity,
    pub sensitivity_y: Sensitivity,
    pub confidence: Confidence,
    pub target_y_offset: TargetYOffset,
    pub max_delta: MaxDelta,
    pub velocity_smoothing: VelocitySmoothing,
    pub fps_limit: FpsLimit,
    pub new_frame_wait: NewFrameWait,
    pub interpolation: Interpolation,
    pub makcu_port: Option<String>,
    pub low_latency: bool,
    #[serde(deserialize_with = "deserialize_hotkeys")]
    pub hotkeys: Vec<Hotkey>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: None,
            display: Display::default(),
            sensitivity_x: Sensitivity::default(),
            sensitivity_y: Sensitivity::default(),
            confidence: Confidence::default(),
            target_y_offset: TargetYOffset::default(),
            max_delta: MaxDelta::default(),
            velocity_smoothing: VelocitySmoothing::default(),
            fps_limit: FpsLimit::default(),
            new_frame_wait: NewFrameWait::default(),
            interpolation: Interpolation::default(),
            makcu_port: None,
            low_latency: false,
            hotkeys: default_hotkeys(),
        }
    }
}

impl Settings {
    /// A file that cannot be parsed is copied to `settings.json.bak` before the defaults replace it.
    pub fn load() -> (Self, Option<String>) {
        match settings_path() {
            Ok(path) => Self::load_from(&path),
            Err(error) => (
                Self::default(),
                Some(format!("Settings could not be read: {error:#}")),
            ),
        }
    }

    fn load_from(path: &Path) -> (Self, Option<String>) {
        tracing::debug!(path = %path.display(), "loading settings");
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None);
            }
            Err(error) => {
                return (
                    Self::default(),
                    Some(format!("Settings could not be read: {error}")),
                );
            }
        };
        match Self::from_json(&bytes) {
            Ok(settings) => (settings, None),
            Err(error) => {
                let backup = back_up(path).map_or_else(
                    |backup_error| format!(" ({backup_error:#})"),
                    |()| String::new(),
                );
                (
                    Self::default(),
                    Some(format!(
                        "Settings could not be loaded. Using defaults; the old file is kept as settings.json.bak{backup}: {error}"
                    )),
                )
            }
        }
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    pub fn save(&self) -> Result<()> {
        self.save_at(&settings_path()?)
    }

    fn save_at(&self, path: &Path) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
        tracing::debug!(path = %path.display(), "settings saved");
        Ok(())
    }

    pub fn bindings_for(&self, action: Action) -> impl Iterator<Item = &Hotkey> {
        self.hotkeys
            .iter()
            .filter(move |binding| binding.action == action)
    }

    pub fn add_binding(&mut self, binding: Hotkey) {
        self.hotkeys.push(binding);
    }

    pub fn remove_binding(&mut self, action: Action, shortcut: Shortcut) {
        self.hotkeys
            .retain(|binding| !(binding.action == action && binding.shortcut == shortcut));
    }
}

fn deserialize_hotkeys<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Hotkey>, D::Error> {
    let entries = Vec::<Value>::deserialize(deserializer)?;
    Ok(entries
        .into_iter()
        .filter_map(|entry| serde_json::from_value::<Hotkey>(entry).ok())
        .collect())
}

fn default_hotkeys() -> Vec<Hotkey> {
    vec![Hotkey {
        action: Action::Hold,
        shortcut: "F8".parse().expect("F8 is a valid shortcut"),
    }]
}

fn back_up(path: &Path) -> Result<()> {
    let mut backup = path.as_os_str().to_owned();
    backup.push(".bak");
    fs::copy(path, &backup).with_context(|| format!("failed to back up {}", path.display()))?;
    Ok(())
}

fn exe_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("failed to find the program's path")?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("the program's path has no folder: {}", exe.display()))
}

pub fn settings_path() -> Result<PathBuf> {
    Ok(exe_dir()?.join("settings.json"))
}

pub fn models_dir() -> Result<PathBuf> {
    Ok(exe_dir()?.join("models"))
}

pub fn logs_dir() -> Result<PathBuf> {
    Ok(exe_dir()?.join("logs"))
}

pub fn models() -> Result<Vec<String>> {
    let dir = models_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let mut names = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() || !is_onnx(&path) {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            names.push(name.to_owned());
        }
    }
    names.sort_by_key(|name| name.to_ascii_lowercase());
    Ok(names)
}

pub fn model_path(name: &str) -> Result<PathBuf> {
    if name.is_empty() {
        bail!("model filename is empty");
    }
    if Path::new(name).components().count() != 1 || !is_onnx(Path::new(name)) {
        bail!("model must be a .onnx filename inside models/");
    }
    let path = models_dir()?.join(name);
    if !path.is_file() {
        bail!("model is not present in models/: {name}");
    }
    Ok(path)
}

fn is_onnx(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("onnx"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nice-aimassist-settings-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_field_gets_its_default_and_the_others_keep_their_values() {
        let settings =
            Settings::from_json(br#"{"sensitivity_x": 250, "low_latency": true}"#).unwrap();
        assert_eq!(settings.sensitivity_x.get(), 250);
        assert_eq!(settings.sensitivity_y, Sensitivity::default());
        assert!(settings.low_latency);
        assert_eq!(settings.confidence, Confidence::default());
        assert_eq!(settings.hotkeys, default_hotkeys());
    }

    #[test]
    fn an_unknown_field_is_dropped_on_the_next_save() {
        let settings = Settings::from_json(br#"{"fps_limit": 120, "version": 2}"#).unwrap();
        let dir = temp_dir("unknown");
        let path = dir.join("settings.json");
        settings.save_at(&path).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("version"));
        assert!(saved.contains("fps_limit"));
    }

    #[test]
    fn a_value_out_of_range_is_clamped() {
        let settings = Settings::from_json(
            br#"{"sensitivity_x": 20000, "new_frame_wait": 0, "target_y_offset": 101}"#,
        )
        .unwrap();
        assert_eq!(settings.sensitivity_x.get(), Sensitivity::MAX);
        assert_eq!(settings.new_frame_wait.get(), NewFrameWait::MIN);
        assert_eq!(settings.target_y_offset.get(), TargetYOffset::MAX);
    }

    #[test]
    fn a_value_of_the_wrong_type_and_invalid_json_are_errors() {
        assert!(Settings::from_json(br#"{"fps_limit": "fast"}"#).is_err());
        assert!(Settings::from_json(br#"{"sensitivity": 150"#).is_err());
        assert!(Settings::from_json(b"[1, 2]").is_err());
    }

    #[test]
    fn an_invalid_hotkey_entry_is_dropped_and_the_others_stay() {
        let settings = Settings::from_json(
            br#"{"hotkeys": [
                {"action": "run_once", "shortcut": "F9"},
                {"action": "continuous_hold", "shortcut": " "},
                {"action": "continuous_hold", "shortcut": "F8"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(settings.hotkeys.len(), 1);
        assert_eq!(settings.hotkeys[0].shortcut.to_string(), "F8");
    }

    #[test]
    fn the_makcu_port_and_an_xinput_binding_round_trip_through_the_file() {
        let settings = Settings {
            makcu_port: Some("COM4".to_owned()),
            hotkeys: vec![Hotkey {
                action: Action::Hold,
                shortcut: "xinput:a".parse().unwrap(),
            }],
            ..Settings::default()
        };
        let restored = Settings::from_json(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
    }

    #[test]
    fn loading_an_unparsable_file_gives_the_defaults_and_keeps_a_backup() {
        let dir = temp_dir("backup");
        let path = dir.join("settings.json");
        fs::write(&path, "{ broken").unwrap();
        let (settings, warning) = Settings::load_from(&path);
        assert_eq!(settings, Settings::default());
        assert!(warning.is_some());
        assert_eq!(
            fs::read_to_string(dir.join("settings.json.bak")).unwrap(),
            "{ broken"
        );
    }

    #[test]
    fn a_missing_file_gives_the_defaults_without_a_warning() {
        let dir = temp_dir("missing");
        let (settings, warning) = Settings::load_from(&dir.join("settings.json"));
        assert_eq!(settings, Settings::default());
        assert!(warning.is_none());
    }

    #[test]
    fn ranged_values_clamp_at_construction() {
        assert_eq!(InterpolationPercent::new(150).get(), 100);
        assert_eq!(Sensitivity::new(0).get(), Sensitivity::MIN);
        assert_eq!(FpsLimit::new(2_000).get(), FpsLimit::MAX);
        assert_eq!(MaxDelta::new(u16::MAX).get(), MaxDelta::MAX);
    }

    #[test]
    fn auto_is_the_255_wire_value_and_percent_is_its_own_value() {
        assert_eq!(Interpolation::Auto.wire_value(), 255);
        assert_eq!(
            Interpolation::Percent(InterpolationPercent::new(40)).wire_value(),
            40
        );
    }

    #[test]
    fn value_methods_give_the_documented_values() {
        assert_eq!(Sensitivity::default().gain(), 1.0);
        assert_eq!(Sensitivity::new(50).gain(), 0.5);
        assert_eq!(TargetYOffset::new(0).center_relative(), -0.5);
        assert_eq!(TargetYOffset::new(50).center_relative(), 0.0);
        assert_eq!(TargetYOffset::new(100).center_relative(), 0.5);
        assert_eq!(MaxDelta::new(MaxDelta::MIN).limit(), None);
        assert_eq!(MaxDelta::new(25).limit(), Some(25.0));
    }

    #[test]
    fn model_path_rejects_a_path_and_a_missing_file() {
        assert!(model_path("nested/yolo.onnx").is_err());
        assert!(model_path("model.txt").is_err());
        assert!(model_path("no-such-model-for-tests.onnx").is_err());
    }
}
