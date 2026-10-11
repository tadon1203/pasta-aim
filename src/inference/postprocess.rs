#[derive(Debug, Clone, Copy)]
pub struct Detection {
    pub center_x: f32,
    pub center_y: f32,
    pub box_width: f32,
    pub box_height: f32,
    pub confidence: f32,
    pub class_id: i32,
}

pub const MAX_CANDIDATES: usize = 16;

/// Every detection that passed the confidence filter, kept for diagnostics.
/// Fixed-size so the per-frame path never allocates.
#[derive(Debug, Clone, Copy)]
pub struct Candidates {
    items: [Detection; MAX_CANDIDATES],
    len: usize,
    total: usize,
}

impl Candidates {
    pub const EMPTY: Self = Self {
        items: [Detection::ZERO; MAX_CANDIDATES],
        len: 0,
        total: 0,
    };

    fn push(&mut self, detection: Detection) {
        if self.len < MAX_CANDIDATES {
            self.items[self.len] = detection;
            self.len += 1;
        }
        self.total += 1;
    }

    /// The first `MAX_CANDIDATES` detections.
    pub fn as_slice(&self) -> &[Detection] {
        &self.items[..self.len]
    }

    /// How many passed the filter, including any beyond `MAX_CANDIDATES`.
    pub fn total(&self) -> usize {
        self.total
    }
}

pub struct Selection {
    pub nearest: Option<Detection>,
    pub candidates: Candidates,
}

impl Detection {
    const ZERO: Self = Self {
        center_x: 0.0,
        center_y: 0.0,
        box_width: 0.0,
        box_height: 0.0,
        confidence: 0.0,
        class_id: 0,
    };
}

/// `data` holds `rows` YOLO rows of `[x1, y1, x2, y2, confidence, class_id]`
/// in either FP32 or FP16.
pub fn select_nearest<T: Copy + Into<f32>>(
    data: &[T],
    rows: usize,
    input_width: usize,
    input_height: usize,
    confidence_threshold: f32,
) -> Selection {
    let image_cx = input_width as f32 * 0.5;
    let image_cy = input_height as f32 * 0.5;
    let mut best: Option<(f32, Detection)> = None;
    let mut candidates = Candidates::EMPTY;

    for row in data.as_chunks::<6>().0.iter().take(rows) {
        // The confidence decides the row first, so a rejected row converts no box values.
        let confidence: f32 = row[4].into();
        if !confidence.is_finite() || confidence < confidence_threshold {
            continue;
        }
        let x1: f32 = row[0].into();
        let y1: f32 = row[1].into();
        let x2: f32 = row[2].into();
        let y2: f32 = row[3].into();
        let class_id: f32 = row[5].into();
        if !x1.is_finite()
            || !y1.is_finite()
            || !x2.is_finite()
            || !y2.is_finite()
            || x2 <= x1
            || y2 <= y1
        {
            continue;
        }

        let center_x = (x1 + x2) * 0.5;
        let center_y = (y1 + y2) * 0.5;
        let dx = center_x - image_cx;
        let dy = center_y - image_cy;
        let distance_sq = dx * dx + dy * dy;
        let detection = Detection {
            center_x,
            center_y,
            box_width: x2 - x1,
            box_height: y2 - y1,
            confidence,
            class_id: class_id.round() as i32,
        };
        candidates.push(detection);
        if !matches!(best, Some((best_distance, _)) if best_distance <= distance_sq) {
            best = Some((distance_sq, detection));
        }
    }
    Selection {
        nearest: best.map(|(_, detection)| detection),
        candidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_valid_detection_wins() {
        let data: [f32; 12] = [
            0.0, 0.0, 20.0, 20.0, 0.95, 1.0, 45.0, 45.0, 55.0, 55.0, 0.80, 2.0,
        ];
        let detection = select_nearest(&data, 2, 100, 100, 0.25)
            .nearest
            .expect("detection");
        assert_eq!(detection.class_id, 2);
        assert_eq!((detection.center_x, detection.center_y), (50.0, 50.0));
        assert_eq!((detection.box_width, detection.box_height), (10.0, 10.0));
    }

    #[test]
    fn fp16_output_matches_fp32() {
        let data: [f32; 12] = [
            0.0, 0.0, 20.0, 20.0, 0.95, 1.0, 45.0, 45.0, 55.0, 55.0, 0.80, 2.0,
        ];
        let half_data = data.map(half::f16::from_f32);
        let detection = select_nearest(&half_data, 2, 100, 100, 0.25)
            .nearest
            .expect("detection");
        assert_eq!(detection.class_id, 2);
        assert_eq!((detection.center_x, detection.center_y), (50.0, 50.0));
    }

    #[test]
    fn rows_below_the_threshold_or_with_a_non_finite_confidence_are_skipped() {
        let data: [f32; 12] = [
            0.0,
            0.0,
            20.0,
            20.0,
            0.10,
            1.0,
            45.0,
            45.0,
            55.0,
            55.0,
            f32::NAN,
            2.0,
        ];
        let selection = select_nearest(&data, 2, 100, 100, 0.25);
        assert!(selection.nearest.is_none());
        assert_eq!(selection.candidates.total(), 0);
    }

    #[test]
    fn a_row_with_non_finite_box_values_is_skipped_even_when_confident() {
        let data: [f32; 12] = [
            f32::INFINITY,
            0.0,
            20.0,
            20.0,
            0.95,
            1.0,
            45.0,
            45.0,
            55.0,
            55.0,
            0.80,
            2.0,
        ];
        let selection = select_nearest(&data, 2, 100, 100, 0.25);
        assert_eq!(selection.candidates.total(), 1);
        assert_eq!(selection.nearest.unwrap().class_id, 2);
    }

    #[test]
    fn all_candidates_above_the_threshold_are_kept() {
        let data: [f32; 18] = [
            0.0, 0.0, 20.0, 20.0, 0.95, 1.0, 45.0, 45.0, 55.0, 55.0, 0.80, 2.0, 0.0, 0.0, 5.0, 5.0,
            0.10, 3.0,
        ];
        let selection = select_nearest(&data, 3, 100, 100, 0.25);
        assert_eq!(selection.candidates.total(), 2);
        assert_eq!(selection.candidates.as_slice().len(), 2);
        assert_eq!(selection.candidates.as_slice()[0].class_id, 1);
    }
}
