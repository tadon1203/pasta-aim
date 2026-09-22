use std::{path::Path, time::Instant};

use anyhow::{Context, Result, bail};
use ort::{
    ep::{self, directml::PerformancePreference},
    session::Session,
    value::TensorRef,
};

use crate::settings::DirectMlDevice;

mod ep_assignment;

#[cfg(test)]
pub(crate) use ep_assignment::EpAssignmentReport;
pub(crate) use ep_assignment::{EpAssignedNode, EpAssignmentState, EpProviderAssignment};

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
    pub preprocess_ms: f64,
    pub inference_ms: f64,
    pub postprocess_ms: f64,
}

pub struct Detector {
    session: Session,
    input_width: usize,
    input_height: usize,
    input_shape: [usize; 4],
    directml_device: DirectMlDevice,
    ep_assignment: EpAssignmentState,
    input: Vec<f32>,
}

impl Detector {
    pub fn load(path: &Path, directml_device: DirectMlDevice) -> Result<Self> {
        tracing::info!(path = %path.display(), ?directml_device, "loading ONNX model");
        if !path.is_file() {
            bail!("model does not exist: {}", path.display());
        }

        let directml = match directml_device {
            DirectMlDevice::AutoHighPerformance => ep::DirectML::default()
                .with_performance_preference(PerformancePreference::HighPerformance),
            DirectMlDevice::Device(index) => ep::DirectML::default().with_device_id(index),
        };
        let providers = [directml.build().error_on_failure()];

        let mut builder = Session::builder()?
            .with_memory_pattern(false)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
            .with_parallel_execution(false)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
            .with_config_entry("session.record_ep_graph_assignment_info", "1")
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
            .with_execution_providers(providers)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let session = builder.commit_from_file(path).with_context(|| {
            format!(
                "failed to load ONNX model with DirectML: {}",
                path.display()
            )
        })?;

        // Assignment recording is diagnostic-only. A successfully initialized session remains usable
        // even if a runtime cannot provide its assignment data.
        let ep_assignment = match ep_assignment::read(&session) {
            Ok(report) => {
                tracing::debug!("ONNX execution-provider assignment report loaded");
                EpAssignmentState::Available(report)
            }
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "ONNX execution-provider assignment report unavailable");
                EpAssignmentState::Unavailable(format!("{error:#}"))
            }
        };

        if session.inputs().len() != 1 {
            bail!("expected exactly one model input");
        }

        let shape = session.inputs()[0]
            .dtype()
            .tensor_shape()
            .context("model input must be a tensor")?;
        let dims: &[i64] = shape.as_ref();
        if dims.len() != 4 || dims[0] != 1 || dims[1] != 3 || dims[2] <= 0 || dims[3] <= 0 {
            bail!(
                "expected a static NCHW input [1, 3, H, W], got {:?}. Export YOLO26 with dynamic=False",
                dims
            );
        }

        let input_height = dims[2] as usize;
        let input_width = dims[3] as usize;
        let pixel_count = input_width
            .checked_mul(input_height)
            .context("model dimensions overflowed")?;
        let input_element_count = pixel_count
            .checked_mul(3)
            .context("model input dimensions overflowed")?;
        tracing::info!(
            width = input_width,
            height = input_height,
            "ONNX model loaded"
        );
        Ok(Self {
            session,
            input_width,
            input_height,
            input_shape: [1, 3, input_height, input_width],
            directml_device,
            ep_assignment,
            input: vec![0.0; input_element_count],
        })
    }

    pub fn input_size(&self) -> (usize, usize) {
        (self.input_width, self.input_height)
    }

    pub fn directml_device(&self) -> DirectMlDevice {
        self.directml_device
    }

    pub fn ep_assignment(&self) -> &EpAssignmentState {
        &self.ep_assignment
    }

    pub fn infer_bgra(&mut self, bgra: &[u8], row_pitch: usize) -> Result<InferenceResult> {
        let preprocess_started = Instant::now();
        preprocess_bgra_rows(
            bgra,
            row_pitch,
            self.input_width,
            self.input_height,
            &mut self.input,
        )?;
        let preprocess_ms = preprocess_started.elapsed().as_secs_f64() * 1000.0;

        let input = TensorRef::from_array_view((self.input_shape, self.input.as_slice()))?;

        let inference_started = Instant::now();
        let outputs = self.session.run(ort::inputs![input])?;
        let inference_ms = inference_started.elapsed().as_secs_f64() * 1000.0;

        let postprocess_started = Instant::now();
        if outputs.len() != 1 {
            bail!("expected exactly one YOLO26 output, got {}", outputs.len());
        }

        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        let dims: &[i64] = shape.as_ref();
        let rows = match dims {
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
            preprocess_ms,
            inference_ms,
            postprocess_ms,
        })
    }
}

fn preprocess_bgra_rows(
    bgra: &[u8],
    row_pitch: usize,
    width: usize,
    height: usize,
    output: &mut [f32],
) -> Result<()> {
    let row_bytes = width.checked_mul(4).context("BGRA row size overflowed")?;
    if row_pitch < row_bytes {
        bail!("BGRA row pitch mismatch: expected at least {row_bytes}, got {row_pitch}");
    }
    let required_bgra = row_pitch
        .checked_mul(height)
        .context("BGRA input length overflowed")?;
    if bgra.len() < required_bgra {
        bail!(
            "BGRA input length mismatch: expected at least {required_bgra}, got {}",
            bgra.len()
        );
    }
    let pixels = width
        .checked_mul(height)
        .context("input pixel count overflowed")?;
    let expected_output = pixels
        .checked_mul(3)
        .context("NCHW buffer length overflowed")?;
    if output.len() != expected_output {
        bail!(
            "NCHW buffer length mismatch: expected {expected_output}, got {}",
            output.len()
        );
    }

    const SCALE: f32 = 1.0 / 255.0;
    for y in 0..height {
        let row_start = y * row_pitch;
        let row = &bgra[row_start..row_start + row_bytes];
        for x in 0..width {
            let pixel = x * 4;
            let index = y * width + x;
            output[index] = row[pixel + 2] as f32 * SCALE;
            output[pixels + index] = row[pixel + 1] as f32 * SCALE;
            output[pixels * 2 + index] = row[pixel] as f32 * SCALE;
        }
    }
    Ok(())
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
    fn bgra_preprocess_skips_row_padding_and_writes_rgb_nchw() {
        let bgra = [
            10_u8, 20, 30, 255, 40, 50, 60, 255, 200, 201, 202, 203, 204, 205, 206, 207, 70, 80,
            90, 255, 100, 110, 120, 255, 208, 209, 210, 211, 212, 213, 214, 215,
        ];
        let mut output = [0.0_f32; 12];
        preprocess_bgra_rows(&bgra, 16, 2, 2, &mut output).unwrap();

        let scale = 1.0 / 255.0;
        assert_eq!(
            output,
            [
                30.0 * scale,
                60.0 * scale,
                90.0 * scale,
                120.0 * scale,
                20.0 * scale,
                50.0 * scale,
                80.0 * scale,
                110.0 * scale,
                10.0 * scale,
                40.0 * scale,
                70.0 * scale,
                100.0 * scale
            ]
        );
    }

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
