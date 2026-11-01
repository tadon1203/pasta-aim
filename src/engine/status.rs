use std::sync::{Arc, Mutex};

use crate::{
    Repaint,
    output::{Device, Port},
    pipeline::{DisplayInfo, Precision},
    sync::MutexExt,
};

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
    pub precision: Option<Precision>,
    pub error: Option<String>,
    /// The nodes that ONNX Runtime assigned to the CPU. `None` when ORT does not report them.
    pub cpu_nodes: Option<usize>,
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
            cpu_nodes: None,
        }
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
    Failed,
}

#[derive(Debug, Clone)]
pub struct MakcuStatus {
    pub phase: MakcuPhase,
    /// The MAKCU of the connected link. `None` while it is not connected.
    pub device: Option<Device>,
    pub error: Option<String>,
    /// The serial ports of the system, listed at each connect.
    pub ports: Vec<Port>,
}

impl Default for MakcuStatus {
    fn default() -> Self {
        Self {
            phase: MakcuPhase::Connecting,
            device: None,
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
            (
                self.makcu
                    .device
                    .as_ref()
                    .is_some_and(|device| device.mouse_route),
                BLOCK_MOUSE_ROUTE,
            ),
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

    fn device(mouse_route: bool) -> Device {
        Device {
            port: "COM4".to_owned(),
            firmware: 4094,
            baud: 4_000_000,
            mouse_route,
        }
    }

    fn ready_state() -> EngineState {
        let mut state = EngineState::default();
        state.model.phase = ModelPhase::Ready;
        state.capture.phase = CapturePhase::Ready;
        state.makcu.phase = MakcuPhase::Connected;
        state.makcu.device = Some(device(true));
        state
    }

    #[test]
    fn ready_when_everything_including_the_mouse_route_is_available() {
        assert!(ready_state().ready());
    }

    #[test]
    fn missing_mouse_route_blocks_output() {
        let mut state = ready_state();
        state.makcu.device = Some(device(false));
        assert_eq!(state.block_reason(), Some("Mouse route is unavailable"));
    }

    #[test]
    fn disconnected_makcu_blocks_output() {
        let mut state = ready_state();
        state.makcu.phase = MakcuPhase::Failed;
        assert_eq!(state.block_reason(), Some("MAKCU is not connected"));
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
