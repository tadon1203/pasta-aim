//! Associates the selected target across consecutive frames by box overlap.

use crate::inference::Detection;

/// Boxes overlapping less than this are treated as different targets.
const MATCH_IOU: f32 = 0.3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Track {
    pub id: u32,
    /// First frame of a new target: after a loss or a jump to another box.
    pub switched: bool,
}

#[derive(Default)]
pub(super) struct TargetTracker {
    last: Option<Detection>,
    next_id: u32,
    current_id: u32,
}

impl TargetTracker {
    pub(super) fn reset(&mut self) {
        self.last = None;
    }

    pub(super) fn update(&mut self, selected: Option<&Detection>) -> Option<Track> {
        let Some(selected) = selected else {
            self.last = None;
            return None;
        };
        let same = self
            .last
            .is_some_and(|last| iou(&last, selected) >= MATCH_IOU);
        if !same {
            self.next_id += 1;
            self.current_id = self.next_id;
        }
        self.last = Some(*selected);
        Some(Track {
            id: self.current_id,
            switched: !same,
        })
    }
}

fn iou(a: &Detection, b: &Detection) -> f32 {
    let ax1 = a.center_x - a.box_width * 0.5;
    let ay1 = a.center_y - a.box_height * 0.5;
    let bx1 = b.center_x - b.box_width * 0.5;
    let by1 = b.center_y - b.box_height * 0.5;
    let overlap_w = (ax1 + a.box_width).min(bx1 + b.box_width) - ax1.max(bx1);
    let overlap_h = (ay1 + a.box_height).min(by1 + b.box_height) - ay1.max(by1);
    if overlap_w <= 0.0 || overlap_h <= 0.0 {
        return 0.0;
    }
    let intersection = overlap_w * overlap_h;
    let union = a.box_width * a.box_height + b.box_width * b.box_height - intersection;
    intersection / union
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(center_x: f32) -> Detection {
        Detection {
            center_x,
            center_y: 50.0,
            box_width: 20.0,
            box_height: 40.0,
            confidence: 0.9,
            class_id: 0,
        }
    }

    #[test]
    fn a_moving_box_keeps_its_id_and_a_jump_or_loss_starts_a_new_one() {
        let mut tracker = TargetTracker::default();
        let first = tracker.update(Some(&detection(50.0))).unwrap();
        assert!(first.switched);

        let moved = tracker.update(Some(&detection(54.0))).unwrap();
        assert_eq!(
            moved,
            Track {
                id: first.id,
                switched: false
            }
        );

        let jumped = tracker.update(Some(&detection(90.0))).unwrap();
        assert!(jumped.switched);
        assert_ne!(jumped.id, first.id);

        assert_eq!(tracker.update(None), None);
        let again = tracker.update(Some(&detection(90.0))).unwrap();
        assert!(again.switched);
        assert_ne!(again.id, jumped.id);
    }

    #[test]
    fn reset_makes_the_next_target_new() {
        let mut tracker = TargetTracker::default();
        tracker.update(Some(&detection(50.0)));
        tracker.reset();
        assert!(tracker.update(Some(&detection(50.0))).unwrap().switched);
    }
}
