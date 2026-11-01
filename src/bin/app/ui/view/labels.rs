use nice_aimassist::{
    engine::{CapturePhase, ModelPhase, OutputPhase},
    output::Device,
};

pub(super) fn model_phase_label(phase: ModelPhase) -> &'static str {
    match phase {
        ModelPhase::Empty => "No active model",
        ModelPhase::Loading => "Loading",
        ModelPhase::Ready => "Ready",
    }
}

pub(super) fn capture_phase_label(phase: CapturePhase) -> &'static str {
    match phase {
        CapturePhase::Initializing => "Initializing",
        CapturePhase::Ready => "Ready",
        CapturePhase::Failed => "Failed",
    }
}

pub(super) fn makcu_phase_label(phase: OutputPhase) -> &'static str {
    match phase {
        OutputPhase::Connecting => "Connecting",
        OutputPhase::Connected => "Connected",
        OutputPhase::Failed => "Disconnected",
    }
}

pub(super) fn makcu_phase_mark(phase: OutputPhase) -> &'static str {
    match phase {
        OutputPhase::Connected => "●",
        OutputPhase::Connecting => "◌",
        OutputPhase::Failed => "○",
    }
}

/// The MAKCU line: the phase, then the port and the firmware once it is connected. The Devices
/// panel and the diagnostics text both show it.
pub(super) fn makcu_line(phase: OutputPhase, device: Option<&Device>) -> String {
    let phase = format!("{} {}", makcu_phase_mark(phase), makcu_phase_label(phase));
    match device {
        Some(device) => format!("{phase} · {} · FW {}", device.port, device.firmware_label()),
        None => phase,
    }
}

pub(super) fn route_label(available: bool) -> &'static str {
    if available {
        "● Ready"
    } else {
        "— Not routed"
    }
}
