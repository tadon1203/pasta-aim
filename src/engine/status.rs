use std::sync::{Arc, Mutex};

use crate::{
    Repaint,
    graphics::DisplayInfo,
    inference::{EpAssignedNode, EpAssignmentState, EpProviderAssignment, TensorPrecision},
    makcu::{MakcuRoutes, PortEntry},
    sync::MutexExt,
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
            .map_or(&[], |provider| provider.nodes.as_slice()),
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
    pub baud: Option<u32>,
    pub routes: MakcuRoutes,
    pub error: Option<String>,
    /// The serial ports of the system, listed at each connect.
    pub ports: Vec<PortEntry>,
}

impl Default for MakcuStatus {
    fn default() -> Self {
        Self {
            phase: MakcuPhase::Connecting,
            port: None,
            firmware: None,
            baud: None,
            routes: MakcuRoutes::default(),
            error: None,
            ports: Vec::new(),
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
}

impl Default for RunStatus {
    fn default() -> Self {
        Self {
            phase: RunPhase::Idle,
            error: None,
        }
    }
}

pub(super) const BLOCK_MODEL: &str = "Model is not ready";
pub(super) const BLOCK_CAPTURE: &str = "DXGI capture is not ready";
pub(super) const BLOCK_MAKCU: &str = "MAKCU is not connected";
pub(super) const BLOCK_MOUSE_ROUTE: &str = "Mouse route is unavailable";

/// The first reason that blocks a run. `checks` is `(ready, reason)` in the order to report.
pub(super) fn run_block_reason(checks: [(bool, &'static str); 4]) -> Option<&'static str> {
    checks
        .into_iter()
        .find_map(|(ready, reason)| (!ready).then_some(reason))
}

/// The state of the engine that the UI shows. It changes on a command or a fault, not on a frame.
#[derive(Debug, Clone, Default)]
pub struct EngineState {
    pub graphics: GraphicsStatus,
    pub model: ModelStatus,
    pub capture: CaptureStatus,
    pub makcu: MakcuStatus,
    pub run: RunStatus,
}

impl EngineState {
    pub fn ready(&self) -> bool {
        self.block_reason().is_none()
    }

    pub fn block_reason(&self) -> Option<&'static str> {
        run_block_reason([
            (self.model.phase == ModelPhase::Ready, BLOCK_MODEL),
            (self.capture.phase == CapturePhase::Ready, BLOCK_CAPTURE),
            (
                self.makcu.phase == MakcuPhase::Connected && self.makcu.error.is_none(),
                BLOCK_MAKCU,
            ),
            (self.makcu.routes.mouse, BLOCK_MOUSE_ROUTE),
        ])
    }
}

/// The engine's own copy of the state. The UI only sees the copies that the engine publishes.
///
/// The engine edits `local` without locking. `flush` publishes a copy when an edit is pending.
pub(super) struct StatusHub {
    local: EngineState,
    shared: Arc<Mutex<Arc<EngineState>>>,
    repaint: Repaint,
    dirty: bool,
}

impl StatusHub {
    pub(super) fn new(shared: Arc<Mutex<Arc<EngineState>>>, repaint: Repaint) -> Self {
        Self {
            local: EngineState::default(),
            shared,
            repaint,
            dirty: false,
        }
    }

    pub(super) fn get(&self) -> &EngineState {
        &self.local
    }

    pub(super) fn edit(&mut self, edit: impl FnOnce(&mut EngineState)) {
        edit(&mut self.local);
        self.dirty = true;
    }

    /// Publishes the pending edit, if there is one, and asks for a repaint.
    pub(super) fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        *self.shared.lock_recover() = Arc::new(self.local.clone());
        self.dirty = false;
        (self.repaint)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::ep_assignment::{
        EpAssignedNode, EpAssignmentReport, EpAssignmentState, EpProviderAssignment,
    };

    fn ready_state() -> EngineState {
        let mut state = EngineState::default();
        state.model.phase = ModelPhase::Ready;
        state.capture.phase = CapturePhase::Ready;
        state.makcu.phase = MakcuPhase::Connected;
        state.makcu.routes.mouse = true;
        state
    }

    #[test]
    fn ready_when_everything_including_the_mouse_route_is_available() {
        assert!(ready_state().ready());
    }

    #[test]
    fn missing_mouse_route_blocks_output() {
        let mut state = ready_state();
        state.makcu.routes.mouse = false;
        assert_eq!(state.block_reason(), Some("Mouse route is unavailable"));
    }

    #[test]
    fn disconnected_makcu_blocks_output() {
        let mut state = ready_state();
        state.makcu.phase = MakcuPhase::Reconnecting;
        assert_eq!(state.block_reason(), Some("MAKCU is not connected"));
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

    #[test]
    fn flush_publishes_only_a_pending_edit() {
        let shared = Arc::new(Mutex::new(Arc::new(EngineState::default())));
        let mut hub = StatusHub::new(Arc::clone(&shared), Arc::new(|| {}));
        hub.flush();
        hub.edit(|state| state.run.phase = RunPhase::Continuous);
        assert_eq!(shared.lock_recover().run.phase, RunPhase::Idle);
        hub.flush();
        assert_eq!(shared.lock_recover().run.phase, RunPhase::Continuous);
    }
}
