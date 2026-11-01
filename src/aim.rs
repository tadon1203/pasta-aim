//! The aim: it tracks the target, works out the mouse move for each frame, and rounds it to whole counts.
//!
//! It knows nothing about MAKCU. The engine owns one `Aim`, and sends the move that it gives to the
//! output.

use crate::{
    pipeline::Detection,
    settings::{MaxDelta, Sensitivity, TargetYOffset},
};

/// Boxes overlapping less than this are treated as different targets.
const MIN_OVERLAP: f32 = 0.3;

/// The mouse movement that the engine sends for one frame, in whole counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move {
    pub x: i16,
    pub y: i16,
}

/// The settings that shape the aim and the move. The engine applies them directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AimSettings {
    pub sensitivity: Sensitivity,
    pub target_y_offset: TargetYOffset,
    pub max_delta: MaxDelta,
}

/// The target of a frame, associated across frames by box overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Track {
    pub id: u64,
    /// First frame of a new target: after a loss or a jump to another box.
    pub switched: bool,
}

/// What one frame gave, for the frame record.
#[derive(Debug, Clone, Copy, Default)]
pub struct Outcome {
    pub track: Option<Track>,
    /// `(dx, dy)` from the capture region center to the aim point, in model input pixels.
    pub error: Option<(f32, f32)>,
    /// The move before rounding.
    pub exact: Option<(f32, f32)>,
    /// The move to send. `None` when the move rounds to zero or there is no target.
    pub rounded: Option<Move>,
}

#[derive(Debug)]
pub struct Aim {
    settings: AimSettings,
    tracker: Tracker,
    /// The remainder carried to the next frame of the same target, so
    /// low gains do not stall inside the dead zone of whole counts.
    carry: (f32, f32),
}

impl Aim {
    pub fn new(settings: AimSettings) -> Self {
        Self {
            settings,
            tracker: Tracker::default(),
            carry: (0.0, 0.0),
        }
    }

    pub fn set_settings(&mut self, settings: AimSettings) {
        self.settings = settings;
    }

    /// Works out the move for one frame.
    pub fn update(&mut self, target: Option<Detection>, input_size: (u32, u32)) -> Outcome {
        let track = self.tracker.update(target.as_ref());
        if track.is_none_or(|track| track.switched) {
            self.carry = (0.0, 0.0);
        }
        let Some(detection) = target else {
            return Outcome {
                track,
                ..Outcome::default()
            };
        };
        let Correction { error, movement } = correct(detection, input_size, self.settings);
        let (rounded, carry) = round_with_carry(movement, self.carry);
        self.carry = carry;
        Outcome {
            track,
            error: Some(error),
            exact: Some(movement),
            rounded,
        }
    }

    /// Clears the track and the remainder.
    pub fn reset(&mut self) {
        self.tracker.reset();
        self.carry = (0.0, 0.0);
    }
}

/// The aim error and the move that one detection asks for, before rounding.
#[derive(Debug, PartialEq)]
struct Correction {
    /// `(dx, dy)` from the capture region center to the aim point.
    error: (f32, f32),
    movement: (f32, f32),
}

/// Rounds `movement + carry` to whole counts and returns the new remainder.
/// `None` when the result is zero (the remainder is still kept by the caller).
fn round_with_carry(movement: (f32, f32), carry: (f32, f32)) -> (Option<Move>, (f32, f32)) {
    let (wx, wy) = (movement.0 + carry.0, movement.1 + carry.1);
    let (x, y) = (round_to_i16(wx), round_to_i16(wy));
    // A clamped (saturated) axis must not leave a huge remainder behind.
    let remainder = (
        (wx - f32::from(x)).clamp(-0.5, 0.5),
        (wy - f32::from(y)).clamp(-0.5, 0.5),
    );
    ((x != 0 || y != 0).then_some(Move { x, y }), remainder)
}

/// Whole counts, clamped to the `i16` range of the wire.
fn round_to_i16(value: f32) -> i16 {
    value
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

fn correct(detection: Detection, input_size: (u32, u32), settings: AimSettings) -> Correction {
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
    Correction {
        error: (dx, dy),
        movement: (sx, sy),
    }
}

/// Associates the selected target across consecutive frames by box overlap.
#[derive(Debug, Default)]
struct Tracker {
    last: Option<Detection>,
    next_id: u64,
    id: u64,
}

impl Tracker {
    fn reset(&mut self) {
        self.last = None;
    }

    fn update(&mut self, selected: Option<&Detection>) -> Option<Track> {
        let Some(selected) = selected else {
            self.last = None;
            return None;
        };
        let same = self
            .last
            .is_some_and(|last| overlap(&last, selected) >= MIN_OVERLAP);
        if !same {
            self.next_id += 1;
            self.id = self.next_id;
        }
        self.last = Some(*selected);
        Some(Track {
            id: self.id,
            switched: !same,
        })
    }
}

/// The overlap of two boxes: the area they share divided by the area they cover together. 0 to 1.
fn overlap(a: &Detection, b: &Detection) -> f32 {
    let ax1 = a.center_x - a.box_width * 0.5;
    let ay1 = a.center_y - a.box_height * 0.5;
    let bx1 = b.center_x - b.box_width * 0.5;
    let by1 = b.center_y - b.box_height * 0.5;
    let overlap_w = (ax1 + a.box_width).min(bx1 + b.box_width) - ax1.max(bx1);
    let overlap_h = (ay1 + a.box_height).min(by1 + b.box_height) - ay1.max(by1);
    if overlap_w <= 0.0 || overlap_h <= 0.0 {
        return 0.0;
    }
    let shared = overlap_w * overlap_h;
    let together = a.box_width * a.box_height + b.box_width * b.box_height - shared;
    shared / together
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(sensitivity: u16, target_y_offset: u16) -> AimSettings {
        AimSettings {
            sensitivity: Sensitivity::new(sensitivity),
            target_y_offset: TargetYOffset::new(target_y_offset),
            max_delta: MaxDelta::default(),
        }
    }

    /// A detection whose center is `center_x` and `center_y`, in a 100 by 100 input.
    fn at(center_x: f32, center_y: f32, box_height: f32) -> Detection {
        Detection {
            box_width: 30.0,
            box_height,
            ..Detection::for_test(center_x, center_y, 0)
        }
    }

    /// Move without carry-over: `None` when it rounds to zero.
    fn rounded_of(correction: &Correction) -> Option<Move> {
        round_with_carry(correction.movement, (0.0, 0.0)).0
    }

    fn movement(x: i16, y: i16) -> Move {
        Move { x, y }
    }

    const INPUT: (u32, u32) = (100, 100);

    #[test]
    fn target_y_offset_picks_a_point_in_the_box_before_sensitivity() {
        // Center is (50, 50); the box center is 10 px right and 20 px down and
        // the box is 40 px tall, so 75% aims 10 px below its center.
        let middle = correct(at(60.0, 70.0, 40.0), INPUT, settings(50, 75));
        assert_eq!(rounded_of(&middle), Some(movement(5, 15)));
        // 0% and 100% aim at the top and bottom edges.
        let top = correct(at(60.0, 70.0, 40.0), INPUT, settings(100, 0));
        assert_eq!(rounded_of(&top), Some(movement(10, 0)));
        let bottom = correct(at(60.0, 70.0, 40.0), INPUT, settings(100, 100));
        assert_eq!(rounded_of(&bottom), Some(movement(10, 40)));
    }

    #[test]
    fn max_delta_caps_movement_length_and_keeps_direction() {
        // Move is (10, 20) at 100% sensitivity; length ~22.4.
        let capped = AimSettings {
            max_delta: MaxDelta::new(10),
            ..settings(100, 50)
        };
        // Scaled to length 10: (4.47, 8.94) -> (4, 9).
        let correction_capped = correct(at(60.0, 70.0, 40.0), INPUT, capped);
        assert_eq!(rounded_of(&correction_capped), Some(movement(4, 9)));
        // A cap above the movement length changes nothing.
        let loose = AimSettings {
            max_delta: MaxDelta::new(100),
            ..capped
        };
        let correction_loose = correct(at(60.0, 70.0, 40.0), INPUT, loose);
        assert_eq!(rounded_of(&correction_loose), Some(movement(10, 20)));
    }

    #[test]
    fn zero_movement_gives_no_move() {
        let off_center = correct(at(60.0, 50.0, 40.0), INPUT, settings(100, 50));
        assert_eq!(rounded_of(&off_center), Some(movement(10, 0)));
        let centered = correct(at(50.0, 50.0, 40.0), INPUT, settings(100, 50));
        assert_eq!(rounded_of(&centered), None);
    }

    #[test]
    fn carry_over_accumulates_sub_count_movement() {
        // 0.3 counts per frame: nothing at first, then a whole count once 0.6 has built up.
        let mut carry = (0.0, 0.0);
        let mut sent = Vec::new();
        for _ in 0..4 {
            let (rounded, next) = round_with_carry((0.3, -0.3), carry);
            carry = next;
            sent.push(rounded);
        }
        assert_eq!(sent, vec![None, Some(movement(1, -1)), None, None]);
        assert!((carry.0 - 0.2).abs() < 1e-5);
    }

    #[test]
    fn saturated_axis_does_not_keep_a_huge_remainder() {
        let (rounded, carry) = round_with_carry((1.0e9, 0.0), (0.0, 0.0));
        assert_eq!(rounded, Some(movement(i16::MAX, 0)));
        assert_eq!(carry, (0.5, 0.0));
    }

    #[test]
    fn a_frame_moves_toward_the_aim_point_by_the_exact_count() {
        let mut aim = Aim::new(settings(100, 50));
        let outcome = aim.update(Some(at(70.0, 50.0, 40.0)), INPUT);
        assert_eq!(outcome.rounded, Some(movement(20, 0)));
        assert_eq!(outcome.error, Some((20.0, 0.0)));
        assert_eq!(outcome.exact, Some((20.0, 0.0)));
    }

    #[test]
    fn a_frame_without_a_target_gives_nothing() {
        let mut aim = Aim::new(settings(100, 50));
        let outcome = aim.update(None, INPUT);
        assert_eq!(outcome.track, None);
        assert_eq!(outcome.rounded, None);
        assert_eq!(outcome.error, None);
    }

    #[test]
    fn the_remainder_goes_to_the_next_frame_of_the_same_track() {
        let mut aim = Aim::new(settings(100, 50));
        // dx is 0.3 per frame: nothing at first, then one count once 0.6 has built up.
        let first = aim.update(Some(at(50.3, 50.0, 40.0)), INPUT);
        assert_eq!(first.rounded, None);
        let second = aim.update(Some(at(50.3, 50.0, 40.0)), INPUT);
        assert_eq!(second.rounded, Some(movement(1, 0)));
    }

    #[test]
    fn a_lost_target_clears_the_remainder() {
        let mut aim = Aim::new(settings(100, 50));
        aim.update(Some(at(50.3, 50.0, 40.0)), INPUT);
        aim.update(None, INPUT);
        let after_loss = aim.update(Some(at(50.3, 50.0, 40.0)), INPUT);
        assert_eq!(after_loss.rounded, None);
    }

    #[test]
    fn reset_clears_the_remainder() {
        let mut aim = Aim::new(settings(100, 50));
        aim.update(Some(at(50.3, 50.0, 40.0)), INPUT);
        aim.reset();
        let after_reset = aim.update(Some(at(50.3, 50.0, 40.0)), INPUT);
        assert_eq!(after_reset.rounded, None);
    }

    #[test]
    fn a_moving_box_keeps_its_id_and_a_jump_or_loss_starts_a_new_one() {
        let mut tracker = Tracker::default();
        let first = tracker.update(Some(&at(50.0, 50.0, 40.0))).unwrap();
        assert!(first.switched);

        let moved = tracker.update(Some(&at(54.0, 50.0, 40.0))).unwrap();
        assert_eq!(
            moved,
            Track {
                id: first.id,
                switched: false
            }
        );

        let jumped = tracker.update(Some(&at(90.0, 50.0, 40.0))).unwrap();
        assert!(jumped.switched);
        assert_ne!(jumped.id, first.id);

        assert_eq!(tracker.update(None), None);
        let again = tracker.update(Some(&at(90.0, 50.0, 40.0))).unwrap();
        assert!(again.switched);
        assert_ne!(again.id, jumped.id);
    }

    #[test]
    fn reset_makes_the_next_target_new() {
        let mut tracker = Tracker::default();
        tracker.update(Some(&at(50.0, 50.0, 40.0)));
        tracker.reset();
        assert!(
            tracker
                .update(Some(&at(50.0, 50.0, 40.0)))
                .unwrap()
                .switched
        );
    }
}
