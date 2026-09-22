use crate::{
    detector::{EpAssignedNode, EpAssignmentState, EpProviderAssignment},
    graphics::{AdapterInfo, DisplayInfo},
    mak::{MakRoutes, PhysicalControllerStatus},
    metrics::PerformanceSnapshot,
    settings::{DirectMlDevice, OutputMode},
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
    pub directml_device: DirectMlDevice,
    pub error: Option<String>,
    pub ep_assignment: Option<EpAssignmentState>,
    pub ep_assignment_summary: Option<EpAssignmentSummary>,
}

impl Default for ModelStatus {
    fn default() -> Self {
        Self {
            phase: ModelPhase::Empty,
            active_name: None,
            loading_name: None,
            input_size: None,
            directml_device: DirectMlDevice::default(),
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
    pub adapters: Vec<AdapterInfo>,
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
pub enum MakPhase {
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

#[derive(Debug, Clone)]
pub struct MakStatus {
    pub phase: MakPhase,
    pub port: Option<String>,
    pub firmware: Option<u32>,
    pub routes: MakRoutes,
    pub physical_controller: PhysicalControllerStatus,
    pub controller_release_pending: bool,
    pub error: Option<String>,
}

impl Default for MakStatus {
    fn default() -> Self {
        Self {
            phase: MakPhase::Connecting,
            port: None,
            firmware: None,
            routes: MakRoutes::default(),
            physical_controller: PhysicalControllerStatus::Unknown,
            controller_release_pending: false,
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
    pub last_command: Option<(i32, i32, OutputMode)>,
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
    pub mak_connected: bool,
    pub mouse_route: bool,
    pub controller_route: bool,
}

pub(super) fn run_block_reason(readiness: RunReadiness, mode: OutputMode) -> Option<&'static str> {
    if !readiness.model_ready {
        return Some("Model is not ready");
    }
    if !readiness.capture_ready {
        return Some("DXGI capture is not ready");
    }
    if !readiness.mak_connected {
        return Some("MAK is not connected");
    }
    match mode {
        OutputMode::Mouse if !readiness.mouse_route => Some("Mouse route is unavailable"),
        OutputMode::Controller if !readiness.controller_route => {
            Some("Controller route is unavailable")
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Default)]
pub struct EngineStatus {
    pub graphics: GraphicsStatus,
    pub model: ModelStatus,
    pub capture: CaptureStatus,
    pub mak: MakStatus,
    pub run: RunStatus,
    pub performance: PerformanceSnapshot,
}

impl EngineStatus {
    fn readiness(&self) -> RunReadiness {
        RunReadiness {
            model_ready: self.model.phase == ModelPhase::Ready,
            capture_ready: self.capture.phase == CapturePhase::Ready,
            mak_connected: self.mak.phase == MakPhase::Connected && self.mak.error.is_none(),
            mouse_route: self.mak.routes.mouse,
            controller_route: self.mak.routes.controller_available(),
        }
    }

    pub fn ready_for(&self, mode: OutputMode) -> bool {
        self.block_reason(mode).is_none()
    }

    pub fn block_reason(&self, mode: OutputMode) -> Option<&'static str> {
        run_block_reason(self.readiness(), mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        detector::{EpAssignedNode, EpAssignmentReport, EpAssignmentState, EpProviderAssignment},
        mak::ControllerKind,
    };

    fn ready_controller_status() -> EngineStatus {
        let mut status = EngineStatus::default();
        status.model.phase = ModelPhase::Ready;
        status.capture.phase = CapturePhase::Ready;
        status.mak.phase = MakPhase::Connected;
        status.mak.routes.controllers.push(ControllerKind::Xbox360);
        status
    }

    #[test]
    fn pending_controller_zero_handoff_does_not_block_active_output() {
        let mut status = ready_controller_status();
        status.mak.controller_release_pending = true;

        assert!(status.ready_for(OutputMode::Controller));
        assert_eq!(status.block_reason(OutputMode::Controller), None);
    }

    #[test]
    fn disconnected_mak_still_blocks_controller_output() {
        let mut status = ready_controller_status();
        status.mak.phase = MakPhase::Reconnecting;
        status.mak.controller_release_pending = true;

        assert!(!status.ready_for(OutputMode::Controller));
        assert_eq!(
            status.block_reason(OutputMode::Controller),
            Some("MAK is not connected")
        );
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
