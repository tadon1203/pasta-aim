use serde::{Deserialize, Serialize};

/// Capture policy shared by persistence, UI, capture and inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureSettings {
    pub wait_ms: u32,
}

impl CaptureSettings {
    pub const WAIT_RANGE: std::ops::RangeInclusive<u32> = 1..=20;

    pub(crate) fn is_valid(self) -> bool {
        Self::WAIT_RANGE.contains(&self.wait_ms)
    }
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self { wait_ms: 2 }
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
        f32::from(self.0) / f32::from(Self::DEFAULT)
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
        f32::from(self.0) / f32::from(Self::MAX)
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

/// Per-frame cap on the output movement length, in command units (after sensitivity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MaxDelta(u16);

impl MaxDelta {
    /// No cap.
    pub const UNLIMITED: u16 = 0;
    pub const DEFAULT: u16 = 512;
    pub const MAX: u16 = i16::MAX as u16;

    pub fn new(value: u16) -> Self {
        Self(value.min(Self::MAX))
    }

    pub fn value(self) -> u16 {
        self.0
    }

    /// The cap, or `None` when unlimited.
    pub fn limit(self) -> Option<f32> {
        (self.0 != Self::UNLIMITED).then_some(f32::from(self.0))
    }

    pub(crate) fn is_valid(self) -> bool {
        self.0 <= Self::MAX
    }
}

impl Default for MaxDelta {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TargetYOffset(i16);

impl TargetYOffset {
    /// Top edge of the target box.
    pub const MIN: i16 = 0;
    /// Center of the target box.
    pub const DEFAULT: i16 = 50;
    /// Bottom edge of the target box.
    pub const MAX: i16 = 100;

    pub fn new(value: i16) -> Self {
        Self(value.clamp(Self::MIN, Self::MAX))
    }

    /// The aim point as a fraction of box height measured from the box center
    /// (-0.5 = top edge, 0.0 = center, 0.5 = bottom edge).
    pub fn center_relative(self) -> f32 {
        (f32::from(self.0) - f32::from(Self::DEFAULT)) / 100.0
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

/// The fixed Move interpolation, in percent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InterpolationPercent(u8);

impl InterpolationPercent {
    pub const MIN: u8 = 0;
    pub const MAX: u8 = 100;

    pub fn new(value: u8) -> Self {
        Self(value.clamp(Self::MIN, Self::MAX))
    }

    pub fn value(self) -> u8 {
        self.0
    }

    pub(crate) fn is_valid(self) -> bool {
        (Self::MIN..=Self::MAX).contains(&self.0)
    }
}

/// The Move interpolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Interpolation {
    /// MAKCU chooses. This is the `INTERPOLATE 255` value.
    #[default]
    Auto,
    Percent(InterpolationPercent),
}

impl Interpolation {
    /// The value of the `INTERPOLATE` command. `Auto` is 255.
    pub fn wire_value(self) -> u8 {
        match self {
            Self::Auto => 255,
            Self::Percent(percent) => percent.value(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_round_trips_through_json() {
        let percent = Interpolation::Percent(InterpolationPercent::new(40));
        assert_eq!(
            serde_json::to_string(&percent).unwrap(),
            r#"{"percent":40}"#
        );
        assert_eq!(
            serde_json::from_str::<Interpolation>(r#"{"percent":40}"#).unwrap(),
            percent
        );
        assert_eq!(
            serde_json::to_string(&Interpolation::Auto).unwrap(),
            r#""auto""#
        );
        assert_eq!(
            serde_json::from_str::<Interpolation>(r#""auto""#).unwrap(),
            Interpolation::Auto
        );
    }

    #[test]
    fn interpolation_percent_is_clamped_at_construction() {
        assert_eq!(InterpolationPercent::new(150).value(), 100);
        assert_eq!(InterpolationPercent::new(0).value(), 0);
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
    fn max_delta_defaults_to_512_zero_is_unlimited_and_large_values_are_clamped() {
        assert_eq!(MaxDelta::default().value(), 512);
        assert_eq!(MaxDelta::new(MaxDelta::UNLIMITED).limit(), None);
        assert_eq!(MaxDelta::new(25).limit(), Some(25.0));
        assert_eq!(MaxDelta::new(u16::MAX).value(), MaxDelta::MAX);
    }

    #[test]
    fn target_y_offset_is_a_box_position_normalized_at_construction() {
        assert_eq!(TargetYOffset::default().value(), 50);
        assert_eq!(TargetYOffset::new(-1).value(), TargetYOffset::MIN);
        assert_eq!(TargetYOffset::new(101).value(), TargetYOffset::MAX);
        assert_eq!(TargetYOffset::new(0).center_relative(), -0.5);
        assert_eq!(TargetYOffset::new(50).center_relative(), 0.0);
        assert_eq!(TargetYOffset::new(100).center_relative(), 0.5);
    }
}
