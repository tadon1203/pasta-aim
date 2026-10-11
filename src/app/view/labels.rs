use crate::{
    engine::{CapturePhase, MakcuPhase, ModelPhase},
    makcu::reject_reason_text,
};

/// The last Raw move reject: its reason code and text, or `None`.
pub(super) fn raw_reason_label(reason: Option<u8>) -> String {
    match reason {
        Some(reason) => format!("{reason}: {}", reject_reason_text(reason)),
        None => "None".to_owned(),
    }
}

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

pub(super) fn makcu_phase_label(phase: MakcuPhase) -> &'static str {
    match phase {
        MakcuPhase::Connecting => "Connecting",
        MakcuPhase::Connected => "Connected",
        MakcuPhase::Reconnecting => "Reconnecting",
        MakcuPhase::Failed => "Disconnected",
    }
}

pub(super) fn makcu_phase_mark(phase: MakcuPhase) -> &'static str {
    match phase {
        MakcuPhase::Connected => "●",
        MakcuPhase::Connecting | MakcuPhase::Reconnecting => "◌",
        MakcuPhase::Failed => "○",
    }
}

pub(super) fn route_label(available: bool) -> &'static str {
    if available {
        "● Ready"
    } else {
        "— Not routed"
    }
}
