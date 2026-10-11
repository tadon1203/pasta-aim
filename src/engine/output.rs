//! Stateful target tracking and mouse output, owned by the engine thread.

use anyhow::Result;

use super::target_track::{TargetTracker, Track};
use crate::{
    inference::Detection,
    mak::MakClient,
    settings::{MaxDelta, Sensitivity, TargetYOffset},
};

/// User-tunable output parameters, applied directly by the engine.
#[derive(Debug, Clone, Copy)]
pub(super) struct OutputSettings {
    pub sensitivity: Sensitivity,
    pub target_y_offset: TargetYOffset,
    pub max_delta: MaxDelta,
}

/// `(dx, dy, confidence, class_id)` relative to the model input center.
pub(super) type TargetInfo = (f32, f32, f32, i32);
/// `(x, y)` actually submitted to MAK.
pub(super) type CommandInfo = (i32, i32);

pub(super) struct OutputOutcome {
    pub target: Option<TargetInfo>,
    pub command: Option<CommandInfo>,
    pub track: Option<Track>,
    /// Movement before rounding (after sensitivity and the max delta cap).
    pub planned: Option<(f32, f32)>,
}

/// Movement wanted for a detection, before rounding.
#[derive(Debug, PartialEq)]
struct Plan {
    target: TargetInfo,
    movement: (f32, f32),
}

/// Rounds `movement + carry` to whole counts and returns the new remainder.
/// `None` when the result is zero (the remainder is still kept by the caller).
fn quantize_with_carry(
    movement: (f32, f32),
    carry: (f32, f32),
) -> (Option<(i32, i32)>, (f32, f32)) {
    let (wx, wy) = (movement.0 + carry.0, movement.1 + carry.1);
    let (x, y) = (quantize(wx), quantize(wy));
    // A clamped (saturated) axis must not leave a huge remainder behind.
    let remainder = (
        (wx - x as f32).clamp(-0.5, 0.5),
        (wy - y as f32).clamp(-0.5, 0.5),
    );
    ((x != 0 || y != 0).then_some((x, y)), remainder)
}

fn quantize(value: f32) -> i32 {
    value.round().clamp(i16::MIN as f32, i16::MAX as f32) as i32
}

fn plan(detection: Detection, input_size: (usize, usize), settings: OutputSettings) -> Plan {
    let dx = detection.center_x - input_size.0 as f32 * 0.5;
    let aim_y =
        detection.center_y + detection.box_height * settings.target_y_offset.center_relative();
    let dy = aim_y - input_size.1 as f32 * 0.5;
    let gain = settings.sensitivity.gain();
    let (mut sx, mut sy) = (dx * gain, dy * gain);
    if let Some(limit) = settings.max_delta.limit() {
        // Scale the vector so its direction is kept while its length is capped.
        let length = sx.hypot(sy);
        if length > limit {
            let scale = limit / length;
            sx *= scale;
            sy *= scale;
        }
    }
    Plan {
        target: (dx, dy, detection.confidence, detection.class_id),
        movement: (sx, sy),
    }
}

pub(super) struct OutputStage {
    settings: OutputSettings,
    input_size: (usize, usize),
    tracker: TargetTracker,
    /// Rounding remainder carried to the next frame of the same target, so
    /// low gains do not stall inside the dead zone of whole counts.
    carry: (f32, f32),
}

impl OutputStage {
    pub(super) fn new(settings: OutputSettings) -> Self {
        Self {
            settings,
            input_size: (0, 0),
            tracker: TargetTracker::default(),
            carry: (0.0, 0.0),
        }
    }

    /// Forgets the previous target and remainder; called when a run starts.
    pub(super) fn reset(&mut self) {
        self.tracker.reset();
        self.carry = (0.0, 0.0);
    }

    pub(super) fn set_settings(&mut self, settings: OutputSettings) {
        self.settings = settings;
    }

    pub(super) fn set_input_size(&mut self, input_size: (usize, usize)) {
        self.input_size = input_size;
    }

    /// Submits the mouse movement for one frame, if there is a target to move to.
    pub(super) fn emit(
        &mut self,
        mak: &mut MakClient,
        detection: Option<Detection>,
        can_send: impl FnOnce() -> bool,
    ) -> Result<OutputOutcome> {
        let track = self.tracker.update(detection.as_ref());
        if track.is_none_or(|track| track.switched) {
            self.carry = (0.0, 0.0);
        }
        let Some(detection) = detection else {
            return Ok(OutputOutcome {
                target: None,
                command: None,
                track,
                planned: None,
            });
        };
        let Plan { target, movement } = plan(detection, self.input_size, self.settings);
        let (step, carry) = quantize_with_carry(movement, self.carry);
        self.carry = carry;
        let Some((x, y)) = step else {
            return Ok(OutputOutcome {
                target: Some(target),
                command: None,
                track,
                planned: Some(movement),
            });
        };
        // Check immediately before the device write.
        if !can_send() {
            self.carry = (0.0, 0.0);
            return Ok(OutputOutcome {
                target: Some(target),
                command: None,
                track,
                planned: Some(movement),
            });
        }
        mak.move_mouse(x, y)?;
        Ok(OutputOutcome {
            target: Some(target),
            command: Some((x, y)),
            track,
            planned: Some(movement),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_frame_is_not_submitted_and_does_not_carry_movement() {
        // A submission would fail because MAK is disconnected. The expiry guard
        // must run before any device call, even for a disconnected device.
        let mut output = OutputStage::new(settings(100, 50));
        output.set_input_size((100, 100));
        let outcome = output
            .emit(&mut MakClient::new(), Some(detection(70.0, 40.0)), || false)
            .unwrap();
        assert_eq!(outcome.command, None);
        assert_eq!(output.carry, (0.0, 0.0));
    }

    fn detection(center_y: f32, box_height: f32) -> Detection {
        Detection {
            center_x: 60.0,
            center_y,
            box_width: 30.0,
            box_height,
            confidence: 0.9,
            class_id: 0,
        }
    }

    fn settings(sensitivity: u16, target_y_offset: i32) -> OutputSettings {
        OutputSettings {
            sensitivity: Sensitivity::new(sensitivity),
            target_y_offset: TargetYOffset::new(target_y_offset),
            max_delta: MaxDelta::default(),
        }
    }

    /// Movement without carry-over: `None` when it rounds to zero.
    fn step_of(plan: &Plan) -> Option<(i32, i32)> {
        quantize_with_carry(plan.movement, (0.0, 0.0)).0
    }

    #[test]
    fn quantize_uses_i16_bounds() {
        assert_eq!(quantize(1.4), 1);
        assert_eq!(quantize(-1.6), -2);
        assert_eq!(quantize(1.0e9), i16::MAX as i32);
        assert_eq!(quantize(-1.0e9), i16::MIN as i32);
    }

    #[test]
    fn target_y_offset_picks_a_point_in_the_box_before_sensitivity() {
        // Center is (50, 50); the box center is 10 px right and 20 px down and
        // the box is 40 px tall, so 75% aims 10 px below its center.
        let plan = plan(detection(70.0, 40.0), (100, 100), settings(50, 75));
        assert_eq!(step_of(&plan), Some((5, 15)));
        // 0% and 100% aim at the top and bottom edges.
        let top = super::plan(detection(70.0, 40.0), (100, 100), settings(100, 0));
        assert_eq!(step_of(&top), Some((10, 0)));
        let bottom = super::plan(detection(70.0, 40.0), (100, 100), settings(100, 100));
        assert_eq!(step_of(&bottom), Some((10, 40)));
    }

    #[test]
    fn max_delta_caps_movement_length_and_keeps_direction() {
        // Movement is (10, 20) at 100% sensitivity; length ~22.4.
        let capped = OutputSettings {
            max_delta: MaxDelta::new(10),
            ..settings(100, 50)
        };
        // Scaled to length 10: (4.47, 8.94) -> (4, 9).
        let plan_capped = plan(detection(70.0, 40.0), (100, 100), capped);
        assert_eq!(step_of(&plan_capped), Some((4, 9)));
        // A cap above the movement length changes nothing.
        let loose = OutputSettings {
            max_delta: MaxDelta::new(100),
            ..capped
        };
        let plan_loose = plan(detection(70.0, 40.0), (100, 100), loose);
        assert_eq!(step_of(&plan_loose), Some((10, 20)));
    }

    #[test]
    fn zero_movement_produces_no_step() {
        let plan_off_center = plan(detection(50.0, 40.0), (100, 100), settings(100, 50));
        assert_eq!(step_of(&plan_off_center), Some((10, 0)));
        let centered = Detection {
            center_x: 50.0,
            ..detection(50.0, 40.0)
        };
        let plan_centered = plan(centered, (100, 100), settings(100, 50));
        assert_eq!(step_of(&plan_centered), None);
    }

    #[test]
    fn carry_over_accumulates_sub_count_movement() {
        // 0.3 counts per frame: nothing at first, then a whole count once 0.6 has built up.
        let mut carry = (0.0, 0.0);
        let mut sent = Vec::new();
        for _ in 0..4 {
            let (step, next) = quantize_with_carry((0.3, -0.3), carry);
            carry = next;
            sent.push(step);
        }
        assert_eq!(sent, vec![None, Some((1, -1)), None, None]);
        assert!((carry.0 - 0.2).abs() < 1e-5);
    }

    #[test]
    fn saturated_axis_does_not_keep_a_huge_remainder() {
        let (step, carry) = quantize_with_carry((1.0e9, 0.0), (0.0, 0.0));
        assert_eq!(step, Some((i16::MAX as i32, 0)));
        assert_eq!(carry, (0.5, 0.0));
    }
}
