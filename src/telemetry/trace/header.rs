//! The header line of the frame trace. It holds what a reader needs to read the file alone.

use serde::Serialize;

use crate::settings::Interpolation;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TraceHeader {
    pub app: AppInfo,
    pub clock: ClockInfo,
    pub run: RunInfo,
    /// `None` when MAKCU is not connected at the start of the run.
    pub makcu: Option<MakcuInfo>,
    pub model: ModelInfo,
    pub settings: SettingsInfo,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AppInfo {
    pub version: &'static str,
    pub build: &'static str,
}

/// The time base of the file. Every time in a line is in microseconds from `run_start_qpc`.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct ClockInfo {
    pub qpc_hz: i64,
    pub run_start_qpc: i64,
    /// The wall clock at the same moment as `run_start_qpc`.
    pub run_start_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunInfo {
    pub generation: u64,
    pub mode: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MakcuInfo {
    pub port: String,
    pub firmware: String,
    pub baud: Option<u32>,
    pub interpolation: Interpolation,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ModelInfo {
    pub name: Option<String>,
    pub input_size: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SettingsInfo {
    pub fps_limit: u16,
    pub capture_wait_ms: u32,
    pub low_latency: bool,
    pub confidence_threshold_percent: u16,
    pub sensitivity_percent: u16,
    pub target_y_offset_percent: u16,
    pub max_delta: u16,
}

#[cfg(test)]
impl TraceHeader {
    /// A header for tests. One tick of the clock is one millisecond, and the run starts at tick 0.
    pub(crate) fn for_test() -> Self {
        Self {
            app: AppInfo {
                version: "0.0.0",
                build: "test",
            },
            clock: ClockInfo {
                qpc_hz: 1_000,
                run_start_qpc: 0,
                run_start_unix_ms: 0,
            },
            run: RunInfo {
                generation: 1,
                mode: "continuous",
            },
            makcu: None,
            model: ModelInfo {
                name: None,
                input_size: Some((640, 640)),
            },
            settings: SettingsInfo {
                fps_limit: 240,
                capture_wait_ms: 2,
                low_latency: false,
                confidence_threshold_percent: 50,
                sensitivity_percent: 100,
                target_y_offset_percent: 50,
                max_delta: 0,
            },
        }
    }
}
