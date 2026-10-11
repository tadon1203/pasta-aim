use std::sync::Arc;

use super::loop_metrics::LoopSnapshot;
use crate::{
    graphics::DisplayInfo,
    inference::{EpAssignedNode, EpAssignmentState, EpProviderAssignment, TensorPrecision},
    makcu::{MakcuRoutes, RawStats},
    metrics::PerformanceSnapshot,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EpAssignmentSummary {
    pub total_nodes: usize,
    pub directml_nodes: usize,
    pub directml_subgraphs: usize,
    pub cpu_nodes: usize,
    pub cpu_subgraphs: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPhase {
    Empty,
    Loading,
    Ready,
}

#[derive(Debug, Clone)]
pub struct ModelStatus {
    pub phase: ModelPhase,
    pub active_name: Option<String>,
    pub loading_name: Option<String>,
    pub input_size: Option<(usize, usize)>,
    pub precision: Option<TensorPrecision>,
    pub error: Option<String>,
    pub ep_assignment: Option<Arc<EpAssignmentState>>,
    pub ep_assignment_summary: Option<EpAssignmentSummary>,
}

impl Default for ModelStatus {
    fn default() -> Self {
        Self {
            phase: ModelPhase::Empty,
            active_name: None,
            loading_name: None,
            input_size: None,
            precision: None,
            error: None,
            ep_assignment: None,
            ep_assignment_summary: None,
        }
    }
}

pub fn ep_assignment_summary(state: &EpAssignmentState) -> Option<EpAssignmentSummary> {
    let EpAssignmentState::Available(report) = state else {
        return None;
    };
    let mut summary = EpAssignmentSummary {
        total_nodes: report.total_nodes,
        ..Default::default()
    };
    for provider in &report.providers {
        match provider.name.as_str() {
            "DmlExecutionProvider" => {
                summary.directml_nodes = provider.nodes.len();
                summary.directml_subgraphs = provider.subgraph_count;
            }
            "CPUExecutionProvider" => {
                summary.cpu_nodes = provider.nodes.len();
                summary.cpu_subgraphs = provider.subgraph_count;
            }
            _ => {}
        }
    }
    Some(summary)
}

pub fn cpu_fallback_nodes(state: &EpAssignmentState) -> &[EpAssignedNode] {
    match state {
        EpAssignmentState::Available(report) => report
            .providers
            .iter()
            .find(|provider| provider.name == "CPUExecutionProvider")
            .map(|provider| provider.nodes.as_slice())
            .unwrap_or(&[]),
        EpAssignmentState::Unavailable(_) => &[],
    }
}

pub fn provider_display_name(provider: &EpProviderAssignment) -> &str {
    match provider.name.as_str() {
        "DmlExecutionProvider" => "DirectML",
        "CPUExecutionProvider" => "CPU",
        _ => &provider.name,
    }
}

#[derive(Debug, Clone, Default)]
pub struct GraphicsStatus {
    pub displays: Vec<DisplayInfo>,
    pub catalog_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePhase {
    Initializing,
    Ready,
    Failed,
}

#[derive(Debug, Clone)]
pub struct CaptureStatus {
    pub phase: CapturePhase,
    pub error: Option<String>,
    pub desktop_size: Option<(usize, usize)>,
    pub selected: Option<DisplayInfo>,
}

impl Default for CaptureStatus {
    fn default() -> Self {
        Self {
            phase: CapturePhase::Initializing,
            error: None,
            desktop_size: None,
            selected: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MakcuPhase {
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

#[derive(Debug, Clone)]
pub struct MakcuStatus {
    pub phase: MakcuPhase,
    pub port: Option<String>,
    pub firmware: Option<u32>,
    pub routes: MakcuRoutes,
    pub raw: RawStats,
    pub error: Option<String>,
}

impl Default for MakcuStatus {
    fn default() -> Self {
        Self {
            phase: MakcuPhase::Connecting,
            port: None,
            firmware: None,
            routes: MakcuRoutes::default(),
            raw: RawStats::default(),
            error: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhase {
    Idle,
    OneShot,
    Continuous,
}

#[derive(Debug, Clone)]
pub struct RunStatus {
    pub phase: RunPhase,
    pub error: Option<String>,
    pub last_target: Option<(f32, f32, f32, i32)>,
    pub last_command: Option<(i32, i32)>,
    pub last_total_ms: Option<f64>,
}

impl Default for RunStatus {
    fn default() -> Self {
        Self {
            phase: RunPhase::Idle,
            error: None,
            last_target: None,
            last_command: None,
            last_total_ms: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RunReadiness {
    pub model_ready: bool,
    pub capture_ready: bool,
    pub makcu_connected: bool,
    pub mouse_route: bool,
}

pub(super) fn run_block_reason(readiness: RunReadiness) -> Option<&'static str> {
    if !readiness.model_ready {
        return Some("Model is not ready");
    }
    if !readiness.capture_ready {
        return Some("DXGI capture is not ready");
    }
    if !readiness.makcu_connected {
        return Some("MAKCU is not connected");
    }
    if !readiness.mouse_route {
        return Some("Mouse route is unavailable");
    }
    None
}

#[derive(Debug, Clone, Default)]
pub struct EngineStatus {
    pub graphics: GraphicsStatus,
    pub model: ModelStatus,
    pub capture: CaptureStatus,
    pub makcu: MakcuStatus,
    pub run: RunStatus,
    pub performance: PerformanceSnapshot,
    pub tracking: LoopSnapshot,
}

impl EngineStatus {
    fn readiness(&self) -> RunReadiness {
        RunReadiness {
            model_ready: self.model.phase == ModelPhase::Ready,
            capture_ready: self.capture.phase == CapturePhase::Ready,
            makcu_connected: self.makcu.phase == MakcuPhase::Connected
                && self.makcu.error.is_none(),
            mouse_route: self.makcu.routes.mouse,
        }
    }

    pub fn ready(&self) -> bool {
        self.block_reason().is_none()
    }

    pub fn block_reason(&self) -> Option<&'static str> {
        run_block_reason(self.readiness())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::ep_assignment::{
        EpAssignedNode, EpAssignmentReport, EpAssignmentState, EpProviderAssignment,
    };

    fn ready_status() -> EngineStatus {
        let mut status = EngineStatus::default();
        status.model.phase = ModelPhase::Ready;
        status.capture.phase = CapturePhase::Ready;
        status.makcu.phase = MakcuPhase::Connected;
        status.makcu.routes.mouse = true;
        status
    }

    #[test]
    fn ready_when_everything_including_the_mouse_route_is_available() {
        assert!(ready_status().ready());
    }

    #[test]
    fn missing_mouse_route_blocks_output() {
        let mut status = ready_status();
        status.makcu.routes.mouse = false;
        assert_eq!(status.block_reason(), Some("Mouse route is unavailable"));
    }

    #[test]
    fn disconnected_makcu_blocks_output() {
        let mut status = ready_status();
        status.makcu.phase = MakcuPhase::Reconnecting;
        assert_eq!(status.block_reason(), Some("MAKCU is not connected"));
    }

    #[test]
    fn assignment_summary_and_cpu_nodes_use_actual_provider_names() {
        let state = EpAssignmentState::Available(EpAssignmentReport {
            total_nodes: 3,
            providers: vec![
                EpProviderAssignment {
                    name: "DmlExecutionProvider".into(),
                    subgraph_count: 1,
                    nodes: vec![
                        EpAssignedNode {
                            name: "conv".into(),
                            domain: String::new(),
                            op_type: "Conv".into()
                        };
                        2
                    ],
                },
                EpProviderAssignment {
                    name: "CPUExecutionProvider".into(),
                    subgraph_count: 1,
                    nodes: vec![EpAssignedNode {
                        name: "shape".into(),
                        domain: String::new(),
                        op_type: "Shape".into(),
                    }],
                },
            ],
        });

        assert_eq!(
            ep_assignment_summary(&state),
            Some(EpAssignmentSummary {
                total_nodes: 3,
                directml_nodes: 2,
                directml_subgraphs: 1,
                cpu_nodes: 1,
                cpu_subgraphs: 1,
            })
        );
        assert_eq!(cpu_fallback_nodes(&state)[0].op_type, "Shape");
    }
}
