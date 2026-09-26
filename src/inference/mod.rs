pub(crate) mod backend;
pub(crate) mod directml;
pub(crate) mod ep_assignment;
pub(crate) mod pipeline;
pub mod postprocess;

use serde::{Deserialize, Serialize};

pub(crate) use ep_assignment::{EpAssignedNode, EpAssignmentState, EpProviderAssignment};
pub use postprocess::Detection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    OrtDirectMl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AdapterLuid {
    pub low: u32,
    pub high: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelInputSpec {
    pub width: usize,
    pub height: usize,
}

#[derive(Debug, Clone)]
pub struct BackendInfo {
    pub kind: BackendKind,
    pub name: String,
    pub adapter_luid: AdapterLuid,
}

pub use backend::{BackendRunResult, InferenceBackend};
