use crate::{
    output::{Device, Port},
    pipeline::{DisplayInfo, Precision},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPhase {
    Empty,
    Loading,
    Ready,
}

#[derive(Debug, Clone)]
pub struct ModelState {
    pub phase: ModelPhase,
    pub active_name: Option<String>,
    pub loading_name: Option<String>,
    pub input_size: Option<(u32, u32)>,
    pub precision: Option<Precision>,
    pub error: Option<String>,
    /// The nodes that ONNX Runtime assigned to the CPU. `None` when ORT does not report them.
    pub cpu_nodes: Option<usize>,
}

impl Default for ModelState {
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
pub struct GraphicsState {
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
pub struct CaptureState {
    pub phase: CapturePhase,
    pub error: Option<String>,
    pub desktop_size: Option<(usize, usize)>,
    pub selected: Option<DisplayInfo>,
}

impl Default for CaptureState {
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
pub enum OutputPhase {
    Connecting,
    Connected,
    Failed,
}

#[derive(Debug, Clone)]
pub struct OutputState {
    pub phase: OutputPhase,
    /// The MAKCU of the connected link. `None` while it is not connected.
    pub device: Option<Device>,
    pub error: Option<String>,
    /// The serial ports of the system, listed at each connect.
    pub ports: Vec<Port>,
}

impl Default for OutputState {
    fn default() -> Self {
        Self {
            phase: OutputPhase::Connecting,
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
pub struct RunState {
    pub phase: RunPhase,
    pub error: Option<String>,
}

impl Default for RunState {
    fn default() -> Self {
        Self {
            phase: RunPhase::Idle,
            error: None,
        }
    }
}

pub(super) const BLOCK_MODEL: &str = "Model is not ready";
pub(super) const BLOCK_CAPTURE: &str = "DXGI capture is not ready";
pub(super) const BLOCK_OUTPUT: &str = "MAKCU is not connected";
pub(super) const BLOCK_MOUSE_ROUTE: &str = "Mouse route is unavailable";

/// The first reason that blocks a run. `checks` is `(ready, reason)` in the order to report.
pub(super) fn run_block_reason(checks: [(bool, &'static str); 4]) -> Option<&'static str> {
    checks
        .into_iter()
        .find_map(|(ready, reason)| (!ready).then_some(reason))
}

/// The state of the engine that the UI shows. It changes on a command or a fault, not on a frame.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub graphics: GraphicsState,
    pub model: ModelState,
    pub capture: CaptureState,
    pub output: OutputState,
    pub run: RunState,
}

impl State {
    /// The first reason that blocks a run: model, capture, output, then mouse route.
    pub fn blocked(&self) -> Option<&'static str> {
        run_block_reason([
            (self.model.phase == ModelPhase::Ready, BLOCK_MODEL),
            (self.capture.phase == CapturePhase::Ready, BLOCK_CAPTURE),
            (
                self.output.phase == OutputPhase::Connected && self.output.error.is_none(),
                BLOCK_OUTPUT,
            ),
            (
                self.output
                    .device
                    .as_ref()
                    .is_some_and(|device| device.mouse_route),
                BLOCK_MOUSE_ROUTE,
            ),
        ])
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

    fn ready_state() -> State {
        let mut state = State::default();
        state.model.phase = ModelPhase::Ready;
        state.capture.phase = CapturePhase::Ready;
        state.output.phase = OutputPhase::Connected;
        state.output.device = Some(device(true));
        state
    }

    #[test]
    fn blocked_reports_model_then_capture_then_output_then_mouse_route() {
        let mut state = ready_state();
        assert_eq!(state.blocked(), None);

        state.output.device = Some(device(false));
        assert_eq!(state.blocked(), Some(BLOCK_MOUSE_ROUTE));

        state.output.phase = OutputPhase::Failed;
        assert_eq!(state.blocked(), Some(BLOCK_OUTPUT));

        state.capture.phase = CapturePhase::Failed;
        assert_eq!(state.blocked(), Some(BLOCK_CAPTURE));

        state.model.phase = ModelPhase::Empty;
        assert_eq!(state.blocked(), Some(BLOCK_MODEL));
    }
}
