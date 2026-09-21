use std::{path::Path, time::Instant};

use anyhow::{Context, Result, bail};
use ort::{ep, session::Session, value::Tensor};

const CONFIDENCE_THRESHOLD: f32 = 0.25;

#[derive(Debug, Clone, Copy)]
pub struct Detection {
    pub center_x: f32,
    pub center_y: f32,
    pub confidence: f32,
    pub class_id: i32,
}

pub struct InferenceResult {
    pub detection: Option<Detection>,
    pub inference_ms: f64,
    pub postprocess_ms: f64,
}

pub struct Detector {
    session: Session,
    input_width: usize,
    input_height: usize,
}

impl Detector {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            bail!("model does not exist: {}", path.display());
        }

        // DirectML is requested explicitly. fail_silently() allows ONNX Runtime
        // to fall back to CPU if the provider is unavailable on this machine.
        let providers = [ep::DirectML::default().build().fail_silently()];
        let mut builder = Session::builder()?
            .with_execution_providers(providers)
            .map_err(ort::Error::<()>::from)?;
        let session = builder
            .commit_from_file(path)
            .with_context(|| format!("failed to load ONNX model {}", path.display()))?;

        if session.inputs().len() != 1 {
            bail!("expected exactly one model input");
        }

        let shape = session.inputs()[0]
            .dtype()
            .tensor_shape()
            .context("model input must be a tensor")?;
        let dims: Vec<i64> = shape.iter().copied().collect();

        if dims.len() != 4 || dims[0] != 1 || dims[1] != 3 || dims[2] <= 0 || dims[3] <= 0 {
            bail!(
                "expected a static NCHW input [1, 3, H, W], got {:?}. Export YOLO26 with dynamic=False",
                dims
            );
        }

        Ok(Self {
            session,
            input_width: dims[3] as usize,
            input_height: dims[2] as usize,
        })
    }

    pub fn input_size(&self) -> (usize, usize) {
        (self.input_width, self.input_height)
    }

    pub fn infer_nearest(&mut self, nchw_rgb: Vec<f32>) -> Result<InferenceResult> {
        let expected = 3 * self.input_width * self.input_height;
        if nchw_rgb.len() != expected {
            bail!(
                "input tensor length mismatch: expected {expected}, got {}",
                nchw_rgb.len()
            );
        }

        let input = Tensor::from_array((
            [1_usize, 3, self.input_height, self.input_width],
            nchw_rgb.into_boxed_slice(),
        ))?;

        let inference_started = Instant::now();
        let outputs = self.session.run(ort::inputs![input])?;
        let inference_ms = inference_started.elapsed().as_secs_f64() * 1000.0;

        let postprocess_started = Instant::now();
        if outputs.len() != 1 {
            bail!("expected exactly one YOLO26 output, got {}", outputs.len());
        }

        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        let dims: Vec<i64> = shape.iter().copied().collect();

        let rows = match dims.as_slice() {
            [1, rows, 6] if *rows > 0 => *rows as usize,
            [rows, 6] if *rows > 0 => *rows as usize,
            _ => {
                bail!(
                    "unsupported YOLO output shape {:?}; this build expects YOLO26 exported with nms=False, producing [1, N, 6]",
                    dims
                )
            }
        };

        if data.len() < rows * 6 {
            bail!("YOLO output buffer is shorter than its reported shape");
        }

        let detection = select_nearest(data, rows, self.input_width, self.input_height);
        let postprocess_ms = postprocess_started.elapsed().as_secs_f64() * 1000.0;
        Ok(InferenceResult {
            detection,
            inference_ms,
            postprocess_ms,
        })
    }
}

fn select_nearest(
    data: &[f32],
    rows: usize,
    input_width: usize,
    input_height: usize,
) -> Option<Detection> {
    let image_cx = input_width as f32 * 0.5;
    let image_cy = input_height as f32 * 0.5;
    let mut best: Option<(f32, Detection)> = None;

    for row in data.chunks_exact(6).take(rows) {
        let x1 = row[0];
        let y1 = row[1];
        let x2 = row[2];
        let y2 = row[3];
        let confidence = row[4];
        let class_id = row[5];

        if !x1.is_finite()
            || !y1.is_finite()
            || !x2.is_finite()
            || !y2.is_finite()
            || !confidence.is_finite()
            || confidence < CONFIDENCE_THRESHOLD
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
            confidence,
            class_id: class_id.round() as i32,
        };

        match best {
            Some((best_distance, _)) if best_distance <= distance_sq => {}
            _ => best = Some((distance_sq, detection)),
        }
    }

    best.map(|(_, detection)| detection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_valid_detection_wins() {
        let data = [
            0.0, 0.0, 20.0, 20.0, 0.95, 1.0, 45.0, 45.0, 55.0, 55.0, 0.80, 2.0,
        ];
        let detection = select_nearest(&data, 2, 100, 100).expect("detection");
        assert_eq!(detection.class_id, 2);
        assert_eq!(detection.center_x, 50.0);
        assert_eq!(detection.center_y, 50.0);
    }

    #[test]
    fn invalid_and_low_confidence_rows_are_ignored() {
        let data = [
            10.0,
            10.0,
            20.0,
            20.0,
            0.10,
            1.0,
            f32::NAN,
            0.0,
            10.0,
            10.0,
            0.99,
            2.0,
        ];
        assert!(select_nearest(&data, 2, 100, 100).is_none());
    }
}
