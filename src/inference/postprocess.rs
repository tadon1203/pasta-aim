#[derive(Debug, Clone, Copy)]
pub struct Detection {
    pub center_x: f32,
    pub center_y: f32,
    pub box_height: f32,
    pub confidence: f32,
    pub class_id: i32,
}

/// `data` holds `rows` YOLO rows of `[x1, y1, x2, y2, confidence, class_id]`
/// in either FP32 or FP16.
pub fn select_nearest<T: Copy + Into<f32>>(
    data: &[T],
    rows: usize,
    input_width: usize,
    input_height: usize,
    confidence_threshold: f32,
) -> Option<Detection> {
    let image_cx = input_width as f32 * 0.5;
    let image_cy = input_height as f32 * 0.5;
    let mut best: Option<(f32, Detection)> = None;

    for row in data.chunks_exact(6).take(rows) {
        let [x1, y1, x2, y2, confidence, class_id] = [
            row[0].into(),
            row[1].into(),
            row[2].into(),
            row[3].into(),
            row[4].into(),
            row[5].into(),
        ];
        if !x1.is_finite()
            || !y1.is_finite()
            || !x2.is_finite()
            || !y2.is_finite()
            || !confidence.is_finite()
            || confidence < confidence_threshold
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
            box_height: y2 - y1,
            confidence,
            class_id: class_id.round() as i32,
        };
        if !matches!(best, Some((best_distance, _)) if best_distance <= distance_sq) {
            best = Some((distance_sq, detection));
        }
    }
    best.map(|(_, detection)| detection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_valid_detection_wins() {
        let data: [f32; 12] = [
            0.0, 0.0, 20.0, 20.0, 0.95, 1.0, 45.0, 45.0, 55.0, 55.0, 0.80, 2.0,
        ];
        let detection = select_nearest(&data, 2, 100, 100, 0.25).expect("detection");
        assert_eq!(detection.class_id, 2);
        assert_eq!((detection.center_x, detection.center_y), (50.0, 50.0));
    }

    #[test]
    fn fp16_output_matches_fp32() {
        let data: [f32; 12] = [
            0.0, 0.0, 20.0, 20.0, 0.95, 1.0, 45.0, 45.0, 55.0, 55.0, 0.80, 2.0,
        ];
        let half_data = data.map(half::f16::from_f32);
        let detection = select_nearest(&half_data, 2, 100, 100, 0.25).expect("detection");
        assert_eq!(detection.class_id, 2);
        assert_eq!((detection.center_x, detection.center_y), (50.0, 50.0));
    }
}
