//! The trace events: what happened in a run and is not a frame.

use serde::Serialize;

use crate::graphics::GpuDropReason;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TraceEvent {
    MakcuDisconnected,
    MakcuConnected {
        port: String,
        firmware: String,
        baud: Option<u32>,
    },
    MakcuInterpolationFailed {
        reason: String,
    },
    /// The next frame record is the first frame.
    PipelineChanged,
    FrameDropped(GpuDropReason),
    /// Messages that did not fit in the Telemetry queue since the last event of this kind.
    TelemetryDropped {
        count: u64,
    },
}

/// The shape of an event line. `event` names the kind, and the other fields belong to that kind.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum EventFields<'a> {
    MakcuDisconnected,
    MakcuConnected {
        port: &'a str,
        firmware: &'a str,
        baud: Option<u32>,
    },
    MakcuInterpolationFailed {
        reason: &'a str,
    },
    PipelineChanged,
    FrameDropped {
        reason: &'static str,
    },
    TelemetryDropped {
        count: u64,
    },
}

impl TraceEvent {
    pub(super) fn fields(&self) -> EventFields<'_> {
        match self {
            Self::MakcuDisconnected => EventFields::MakcuDisconnected,
            Self::MakcuConnected {
                port,
                firmware,
                baud,
            } => EventFields::MakcuConnected {
                port,
                firmware,
                baud: *baud,
            },
            Self::MakcuInterpolationFailed { reason } => {
                EventFields::MakcuInterpolationFailed { reason }
            }
            Self::PipelineChanged => EventFields::PipelineChanged,
            Self::FrameDropped(reason) => EventFields::FrameDropped {
                reason: reason.name(),
            },
            Self::TelemetryDropped { count } => EventFields::TelemetryDropped { count: *count },
        }
    }
}
