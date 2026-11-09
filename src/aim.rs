use crate::{
    pipeline::Detection,
    settings::{MaxDelta, Sensitivity, TargetYOffset, VelocitySmoothing},
};

const DELAY_BLEND: f32 = 0.2;

const MAX_AUTO_LEAD: f32 = 0.2;

const LEAD_EXTRA: f32 = 0.01;

const MIN_OVERLAP: f32 = 0.3;

const MAX_FRAME_GAP: f64 = 0.1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move {
    pub x: i16,
    pub y: i16,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AimSettings {
    pub sensitivity_x: Sensitivity,
    pub sensitivity_y: Sensitivity,
    pub target_y_offset: TargetYOffset,
    pub max_delta: MaxDelta,
    pub velocity_smoothing: VelocitySmoothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Track {
    pub id: u64,
    pub switched: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Outcome {
    pub error: Option<(f32, f32)>,
    #[cfg(test)]
    pub exact: Option<(f32, f32)>,
    pub rounded: Option<Move>,
}

#[derive(Debug)]
pub struct Aim {
    settings: AimSettings,
    tracker: Tracker,
    carry: (f32, f32),
    motion: Motion,
    delay: Option<f32>,
}

impl Aim {
    pub fn new(settings: AimSettings) -> Self {
        Self {
            settings,
            tracker: Tracker::default(),
            carry: (0.0, 0.0),
            motion: Motion::default(),
            delay: None,
        }
    }

    pub fn set_settings(&mut self, settings: AimSettings) {
        self.settings = settings;
    }

    pub fn observe_delay(&mut self, seconds: f64) {
        let seconds = seconds.max(0.0) as f32;
        self.delay = Some(
            self.delay
                .map_or(seconds, |delay| delay + DELAY_BLEND * (seconds - delay)),
        );
    }

    fn lead_seconds(&self) -> f32 {
        (self.delay.unwrap_or(0.0) + LEAD_EXTRA).min(MAX_AUTO_LEAD)
    }

    pub fn update(
        &mut self,
        target: Option<Detection>,
        input_size: (u32, u32),
        at: f64,
    ) -> Outcome {
        let track = self.tracker.update(target.as_ref());
        if track.is_none_or(|track| track.switched) {
            self.carry = (0.0, 0.0);
            self.motion = Motion::default();
        }
        let Some(detection) = target else {
            return Outcome::default();
        };
        let velocity =
            self.motion
                .observe(&detection, at, self.settings.velocity_smoothing.blend());
        let aimed = led(detection, velocity, self.lead_seconds());
        let Correction { error, movement } = correct(aimed, input_size, self.settings);
        let (rounded, carry) = round_with_carry(movement, self.carry);
        self.carry = carry;
        self.motion.sent(
            rounded,
            (
                self.settings.sensitivity_x.gain(),
                self.settings.sensitivity_y.gain(),
            ),
        );
        Outcome {
            error: Some(error),
            #[cfg(test)]
            exact: Some(movement),
            rounded,
        }
    }

    pub fn reset(&mut self) {
        self.tracker.reset();
        self.carry = (0.0, 0.0);
        self.motion = Motion::default();
        self.delay = None;
    }
}

#[derive(Debug, Clone, Copy)]
struct Seen {
    center: (f32, f32),
    at: f64,
}

#[derive(Debug, Default)]
struct Motion {
    velocity: (f32, f32),
    previous: Option<Seen>,
    shift: (f32, f32),
}

impl Motion {
    fn observe(&mut self, detection: &Detection, at: f64, blend: f32) -> (f32, f32) {
        let center = (detection.center_x, detection.center_y);
        if let Some(previous) = self.previous {
            let gap = at - previous.at;
            if gap > 0.0 && gap <= MAX_FRAME_GAP {
                let gap = gap as f32;
                let raw = (
                    (center.0 - previous.center.0 + self.shift.0) / gap,
                    (center.1 - previous.center.1 + self.shift.1) / gap,
                );
                self.velocity.0 += blend * (raw.0 - self.velocity.0);
                self.velocity.1 += blend * (raw.1 - self.velocity.1);
            } else {
                self.velocity = (0.0, 0.0);
            }
        }
        self.previous = Some(Seen { center, at });
        self.velocity
    }

    fn sent(&mut self, rounded: Option<Move>, gain: (f32, f32)) {
        self.shift = rounded.map_or((0.0, 0.0), |sent| {
            (f32::from(sent.x) / gain.0, f32::from(sent.y) / gain.1)
        });
    }
}

fn led(detection: Detection, velocity: (f32, f32), seconds: f32) -> Detection {
    let ahead = |speed: f32, size: f32| (speed * seconds).min(size).max(-size);
    Detection {
        center_x: detection.center_x + ahead(velocity.0, detection.box_width.abs()),
        center_y: detection.center_y + ahead(velocity.1, detection.box_height.abs()),
        ..detection
    }
}

#[derive(Debug, PartialEq)]
struct Correction {
    error: (f32, f32),
    movement: (f32, f32),
}

/// The remainder is kept even when the move is `None`.
fn round_with_carry(movement: (f32, f32), carry: (f32, f32)) -> (Option<Move>, (f32, f32)) {
    let (wx, wy) = (movement.0 + carry.0, movement.1 + carry.1);
    let (x, y) = (round_to_i16(wx), round_to_i16(wy));
    // A saturated axis must not leave a huge remainder.
    let remainder = (
        (wx - f32::from(x)).clamp(-0.5, 0.5),
        (wy - f32::from(y)).clamp(-0.5, 0.5),
    );
    ((x != 0 || y != 0).then_some(Move { x, y }), remainder)
}

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
    let (mut sx, mut sy) = (
        dx * settings.sensitivity_x.gain(),
        dy * settings.sensitivity_y.gain(),
    );
    if let Some(limit) = settings.max_delta.limit() {
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
            sensitivity_x: Sensitivity::new(sensitivity),
            sensitivity_y: Sensitivity::new(sensitivity),
            target_y_offset: TargetYOffset::new(target_y_offset),
            max_delta: MaxDelta::default(),
            velocity_smoothing: VelocitySmoothing::new(0),
        }
    }

    fn leading(lead_ms: u16) -> Aim {
        leading_with(settings(100, 50), lead_ms)
    }

    fn leading_with(settings: AimSettings, lead_ms: u16) -> Aim {
        let mut aim = Aim::new(settings);
        aim.delay = Some(f32::from(lead_ms) / 1000.0 - LEAD_EXTRA);
        aim
    }

    fn at(center_x: f32, center_y: f32, box_height: f32) -> Detection {
        Detection {
            box_width: 30.0,
            box_height,
            ..Detection::for_test(center_x, center_y, 0)
        }
    }

    fn rounded_of(correction: &Correction) -> Option<Move> {
        round_with_carry(correction.movement, (0.0, 0.0)).0
    }

    fn movement(x: i16, y: i16) -> Move {
        Move { x, y }
    }

    const INPUT: (u32, u32) = (100, 100);

    #[test]
    fn target_y_offset_picks_a_point_in_the_box_before_sensitivity() {
        let middle = correct(at(60.0, 70.0, 40.0), INPUT, settings(50, 75));
        assert_eq!(rounded_of(&middle), Some(movement(5, 15)));
        let top = correct(at(60.0, 70.0, 40.0), INPUT, settings(100, 0));
        assert_eq!(rounded_of(&top), Some(movement(10, 0)));
        let bottom = correct(at(60.0, 70.0, 40.0), INPUT, settings(100, 100));
        assert_eq!(rounded_of(&bottom), Some(movement(10, 40)));
    }

    #[test]
    fn each_axis_has_its_own_sensitivity() {
        let split = AimSettings {
            sensitivity_x: Sensitivity::new(50),
            sensitivity_y: Sensitivity::new(200),
            ..settings(100, 50)
        };
        let correction = correct(at(60.0, 70.0, 40.0), INPUT, split);
        assert_eq!(rounded_of(&correction), Some(movement(5, 40)));
    }

    #[test]
    fn max_delta_caps_movement_length_and_keeps_direction() {
        let capped = AimSettings {
            max_delta: MaxDelta::new(10),
            ..settings(100, 50)
        };
        let correction_capped = correct(at(60.0, 70.0, 40.0), INPUT, capped);
        assert_eq!(rounded_of(&correction_capped), Some(movement(4, 9)));
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
        let outcome = aim.update(Some(at(70.0, 50.0, 40.0)), INPUT, 0.0);
        assert_eq!(outcome.rounded, Some(movement(20, 0)));
        assert_eq!(outcome.error, Some((20.0, 0.0)));
        assert_eq!(outcome.exact, Some((20.0, 0.0)));
    }

    #[test]
    fn a_frame_without_a_target_gives_nothing() {
        let mut aim = Aim::new(settings(100, 50));
        let outcome = aim.update(None, INPUT, 0.0);
        assert_eq!(outcome.rounded, None);
        assert_eq!(outcome.error, None);
    }

    #[test]
    fn the_remainder_goes_to_the_next_frame_of_the_same_track() {
        let mut aim = Aim::new(settings(100, 50));
        let first = aim.update(Some(at(50.3, 50.0, 40.0)), INPUT, 0.0);
        assert_eq!(first.rounded, None);
        let second = aim.update(Some(at(50.3, 50.0, 40.0)), INPUT, 0.0);
        assert_eq!(second.rounded, Some(movement(1, 0)));
    }

    #[test]
    fn a_lost_target_clears_the_remainder() {
        let mut aim = Aim::new(settings(100, 50));
        aim.update(Some(at(50.3, 50.0, 40.0)), INPUT, 0.0);
        aim.update(None, INPUT, 0.0);
        let after_loss = aim.update(Some(at(50.3, 50.0, 40.0)), INPUT, 0.0);
        assert_eq!(after_loss.rounded, None);
    }

    #[test]
    fn reset_clears_the_remainder() {
        let mut aim = Aim::new(settings(100, 50));
        aim.update(Some(at(50.3, 50.0, 40.0)), INPUT, 0.0);
        aim.reset();
        let after_reset = aim.update(Some(at(50.3, 50.0, 40.0)), INPUT, 0.0);
        assert_eq!(after_reset.rounded, None);
    }

    #[test]
    fn the_aim_point_leads_a_moving_target_by_its_velocity() {
        let mut aim = leading(20);
        let first = aim.update(Some(at(50.0, 50.0, 40.0)), INPUT, 0.0);
        assert_eq!(first.exact, Some((0.0, 0.0)));
        let moved = aim.update(Some(at(60.0, 50.0, 40.0)), INPUT, 0.01);
        let (x, y) = moved.exact.unwrap();
        assert!((x - 30.0).abs() < 1e-3, "x was {x}");
        assert_eq!(y, 0.0);
    }

    fn x_move_of_second_frame(aim: &mut Aim) -> f32 {
        aim.update(Some(at(50.0, 50.0, 40.0)), INPUT, 0.0);
        aim.update(Some(at(60.0, 50.0, 40.0)), INPUT, 0.01)
            .exact
            .unwrap()
            .0
    }

    #[test]
    fn the_lead_follows_the_measured_delay_plus_the_extra() {
        let mut aim = Aim::new(settings(100, 50));
        aim.observe_delay(0.010);
        assert!((x_move_of_second_frame(&mut aim) - 30.0).abs() < 1e-3);

        aim.reset();
        aim.observe_delay(0.010);
        aim.observe_delay(0.060);
        assert!((aim.delay.unwrap() - 0.020).abs() < 1e-6);
    }

    #[test]
    fn the_lead_without_a_measurement_is_the_extra() {
        let mut aim = Aim::new(settings(100, 50));
        assert!((x_move_of_second_frame(&mut aim) - 20.0).abs() < 1e-3);
    }

    #[test]
    fn the_lead_is_capped() {
        let mut aim = Aim::new(settings(100, 50));
        aim.observe_delay(5.0);
        assert!((aim.lead_seconds() - MAX_AUTO_LEAD).abs() < 1e-6);
    }

    #[test]
    fn the_lead_is_at_most_one_box_size() {
        let mut aim = leading(200);
        aim.update(Some(at(50.0, 50.0, 40.0)), INPUT, 0.0);
        let (x, _) = aim
            .update(Some(at(60.0, 50.0, 40.0)), INPUT, 0.01)
            .exact
            .unwrap();
        assert!((x - 40.0).abs() < 1e-3, "x was {x}");
    }

    #[test]
    fn the_shift_of_our_own_move_is_not_a_target_velocity() {
        let mut aim = leading(100);
        let first = aim.update(Some(at(60.0, 50.0, 40.0)), INPUT, 0.0);
        assert_eq!(first.rounded, Some(movement(10, 0)));
        let second = aim.update(Some(at(50.0, 50.0, 40.0)), INPUT, 0.01);
        assert_eq!(second.exact, Some((0.0, 0.0)));
    }

    #[test]
    fn smoothing_blends_the_new_velocity_into_the_estimate() {
        let smooth = AimSettings {
            velocity_smoothing: VelocitySmoothing::new(50),
            ..settings(100, 50)
        };
        let mut aim = leading_with(smooth, 20);
        aim.update(Some(at(50.0, 50.0, 40.0)), INPUT, 0.0);
        let (x, _) = aim
            .update(Some(at(60.0, 50.0, 40.0)), INPUT, 0.01)
            .exact
            .unwrap();
        assert!((x - 20.0).abs() < 1e-3, "x was {x}");
    }

    #[test]
    fn a_long_gap_a_switch_and_a_reset_give_no_lead() {
        let still =
            |aim: &mut Aim, x: f32, at_s: f64| aim.update(Some(at(x, 50.0, 40.0)), INPUT, at_s);

        let mut gap = leading(20);
        still(&mut gap, 50.0, 0.0);
        let exact = still(&mut gap, 60.0, 1.0).exact;
        assert_eq!(exact, Some((10.0, 0.0)));

        let mut switched = leading(20);
        still(&mut switched, 50.0, 0.0);
        let exact = still(&mut switched, 95.0, 0.01).exact;
        assert_eq!(exact, Some((45.0, 0.0)));

        let mut reset = leading(20);
        still(&mut reset, 50.0, 0.0);
        reset.reset();
        let exact = still(&mut reset, 60.0, 0.01).exact;
        assert_eq!(exact, Some((10.0, 0.0)));
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
