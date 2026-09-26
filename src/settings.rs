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

/// Per-frame cap on the output movement length, in command units (after sensitivity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MaxDelta(u16);

impl MaxDelta {
    /// No cap.
    pub const UNLIMITED: u16 = 0;
    pub const DEFAULT: u16 = Self::UNLIMITED;
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

    pub fn new(value: i32) -> Self {
        Self(value.clamp(i32::from(Self::MIN), i32::from(Self::MAX)) as i16)
    }

    /// Aim point as a fraction of box height measured from the box center
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

/// Stroke width of the overlay box, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OverlayLineWidth(u8);

impl OverlayLineWidth {
    pub const MIN: u8 = 1;
    pub const DEFAULT: u8 = 2;
    pub const MAX: u8 = 8;

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

impl Default for OverlayLineWidth {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

/// Outline drawn outside the box stroke, in physical pixels; 0 = no outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OverlayOutlineWidth(u8);

impl OverlayOutlineWidth {
    /// 0 draws no outline.
    pub const DEFAULT: u8 = 1;
    pub const MAX: u8 = 4;

    pub fn new(value: u8) -> Self {
        Self(value.min(Self::MAX))
    }

    pub fn value(self) -> u8 {
        self.0
    }

    pub(crate) fn is_valid(self) -> bool {
        self.0 <= Self::MAX
    }
}

impl Default for OverlayOutlineWidth {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

/// Overlay opacity as a percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OverlayOpacity(u8);

impl OverlayOpacity {
    pub const MIN: u8 = 10;
    pub const DEFAULT: u8 = 100;
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

impl Default for OverlayOpacity {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

/// Bounding box overlay drawn over the model input region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayConfig {
    pub enabled: bool,
    pub line_width: OverlayLineWidth,
    pub color: [u8; 3],
    pub opacity: OverlayOpacity,
    pub outline_width: OverlayOutlineWidth,
    pub outline_color: [u8; 3],
}

impl OverlayConfig {
    pub(crate) fn is_valid(&self) -> bool {
        self.line_width.is_valid() && self.opacity.is_valid() && self.outline_width.is_valid()
    }
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            line_width: OverlayLineWidth::default(),
            color: [255, 64, 64],
            opacity: OverlayOpacity::default(),
            outline_width: OverlayOutlineWidth::default(),
            outline_color: [0, 0, 0],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_style_values_are_normalized_at_construction() {
        assert_eq!(OverlayLineWidth::new(0).value(), OverlayLineWidth::MIN);
        assert_eq!(OverlayLineWidth::new(99).value(), OverlayLineWidth::MAX);
        assert_eq!(OverlayOutlineWidth::new(0).value(), 0);
        assert_eq!(
            OverlayOutlineWidth::new(99).value(),
            OverlayOutlineWidth::MAX
        );
        assert_eq!(OverlayOpacity::new(0).value(), OverlayOpacity::MIN);
        assert_eq!(OverlayOpacity::new(200).ratio(), 1.0);
        assert!(OverlayConfig::default().is_valid());
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
    fn max_delta_zero_is_unlimited_and_large_values_are_clamped() {
        assert_eq!(MaxDelta::default().limit(), None);
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
