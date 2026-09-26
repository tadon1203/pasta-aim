use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InjectionApi {
    #[default]
    MakApi,
    KmApi,
}

impl InjectionApi {
    pub fn label(self) -> &'static str {
        match self {
            Self::MakApi => "MAK_API",
            Self::KmApi => "KM_API",
        }
    }
}

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
#[serde(deny_unknown_fields)]
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

    pub(crate) fn is_valid(self) -> bool {
        (Self::MIN..=Self::MAX).contains(&self.0)
    }
}

impl Default for Sensitivity {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConfidenceThreshold(u8);

impl ConfidenceThreshold {
    pub const MIN: u8 = 0;
    pub const DEFAULT: u8 = 50;
    pub const MAX: u8 = 100;

    pub fn new(value: u8) -> Self {
        Self(value.clamp(Self::MIN, Self::MAX))
    }

    pub fn value(self) -> u8 {
        self.0
    }

    pub fn ratio(self) -> f32 {
        self.0 as f32 / Self::MAX as f32
    }

    pub(crate) fn is_valid(self) -> bool {
        (Self::MIN..=Self::MAX).contains(&self.0)
    }
}

impl Default for ConfidenceThreshold {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TargetYOffset(i16);

impl TargetYOffset {
    pub const MIN: i16 = -100;
    pub const DEFAULT: i16 = 0;
    pub const MAX: i16 = 100;

    pub fn new(value: i32) -> Self {
        Self(value.clamp(i32::from(Self::MIN), i32::from(Self::MAX)) as i16)
    }

    pub fn value(self) -> i16 {
        self.0
    }

    pub(crate) fn is_valid(self) -> bool {
        (Self::MIN..=Self::MAX).contains(&self.0)
    }
}

impl Default for TargetYOffset {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FpsLimit(u16);

impl FpsLimit {
    pub const MIN: u16 = 1;
    pub const DEFAULT: u16 = 240;
    pub const MAX: u16 = 1_000;

    pub fn new(value: u16) -> Self {
        Self(value.clamp(Self::MIN, Self::MAX))
    }

    pub fn value(self) -> u16 {
        self.0
    }

    pub(crate) fn is_valid(self) -> bool {
        (Self::MIN..=Self::MAX).contains(&self.0)
    }
}

impl Default for FpsLimit {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitivity_is_integer_and_normalized_at_construction() {
        assert_eq!(Sensitivity::new(0).value(), 1);
        assert_eq!(Sensitivity::default().gain(), 1.0);
        assert_eq!(Sensitivity::new(50).gain(), 0.5);
        assert_eq!(Sensitivity::new(200).gain(), 2.0);
        assert_eq!(Sensitivity::new(20_000).value(), 10_000);
    }

    #[test]
    fn fps_limit_is_integer_and_normalized_at_construction() {
        assert_eq!(FpsLimit::new(0).value(), FpsLimit::MIN);
        assert_eq!(FpsLimit::default().value(), 240);
        assert_eq!(FpsLimit::new(2_000).value(), FpsLimit::MAX);
    }

    #[test]
    fn confidence_threshold_is_percentage_and_normalized_at_construction() {
        assert_eq!(ConfidenceThreshold::default().value(), 50);
        assert_eq!(ConfidenceThreshold::default().ratio(), 0.5);
        assert_eq!(ConfidenceThreshold::new(0).ratio(), 0.0);
        assert_eq!(ConfidenceThreshold::new(100).ratio(), 1.0);
        assert_eq!(ConfidenceThreshold::new(101).value(), 100);
    }

    #[test]
    fn target_y_offset_is_signed_and_normalized_at_construction() {
        assert_eq!(TargetYOffset::default().value(), 0);
        assert_eq!(TargetYOffset::new(-101).value(), TargetYOffset::MIN);
        assert_eq!(TargetYOffset::new(101).value(), TargetYOffset::MAX);
    }
}
