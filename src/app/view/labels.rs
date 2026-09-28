use crate::engine::{CapturePhase, MakPhase, ModelPhase};

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

pub(super) fn mak_phase_label(phase: MakPhase) -> &'static str {
    match phase {
        MakPhase::Connecting => "Connecting",
        MakPhase::Connected => "Connected",
        MakPhase::Reconnecting => "Reconnecting",
        MakPhase::Failed => "Disconnected",
    }
}

pub(super) fn mak_phase_mark(phase: MakPhase) -> &'static str {
    match phase {
        MakPhase::Connected => "●",
        MakPhase::Connecting | MakPhase::Reconnecting => "◌",
        MakPhase::Failed => "○",
    }
}

pub(super) fn route_label(available: bool) -> &'static str {
    if available {
        "● Ready"
    } else {
        "— Not routed"
    }
}
