pub(crate) mod directml;
pub mod ep_assignment;
pub mod postprocess;

use ort::value::TensorElementType;
use serde::{Deserialize, Serialize};

pub use ep_assignment::{EpAssignedNode, EpAssignmentState, EpProviderAssignment};
pub use postprocess::{Candidates, Detection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AdapterLuid {
    pub low: u32,
    pub high: i32,
}

/// Floating point element type of a model's input tensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TensorPrecision {
    Fp16,
    Fp32,
}

impl TensorPrecision {
    pub fn from_element_type(element_type: TensorElementType) -> Option<Self> {
        match element_type {
            TensorElementType::Float16 => Some(Self::Fp16),
            TensorElementType::Float32 => Some(Self::Fp32),
            _ => None,
        }
    }

    pub fn element_bytes(self) -> usize {
        match self {
            Self::Fp16 => std::mem::size_of::<half::f16>(),
            Self::Fp32 => std::mem::size_of::<f32>(),
        }
    }

    /// Number of 32-bit words needed to hold `elements` tensor elements.
    pub fn packed_words(self, elements: usize) -> Option<usize> {
        Some(elements.checked_mul(self.element_bytes())?.div_ceil(4))
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Fp16 => "FP16",
            Self::Fp32 => "FP32",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelInputSpec {
    pub width: usize,
    pub height: usize,
    pub precision: TensorPrecision,
}
