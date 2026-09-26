use anyhow::Result;

use super::{BackendInfo, Detection, ModelInputSpec};
use crate::settings::ConfidenceThreshold;

pub struct BackendRunResult {
    pub detection: Option<Detection>,
    pub ort_run_wall_ms: f64,
    pub postprocess_ms: f64,
}

pub trait InferenceBackend {
    fn info(&self) -> &BackendInfo;
    fn input_spec(&self) -> ModelInputSpec;
    fn run(&mut self) -> Result<BackendRunResult>;
    fn set_confidence_threshold(&mut self, _value: ConfidenceThreshold) {}
    fn shutdown(&mut self) {}
}
