//! MAKCU connection lifecycle.

use super::Worker;
use crate::{
    engine::MakcuPhase,
    output::{self, Device},
    telemetry::TraceEvent,
};

impl Worker {
    /// Connects again: the MAKCU ports in order, or only the one that the user chose. A run stops first.
    pub(super) fn reconnect_makcu(&mut self) {
        tracing::info!("reconnecting MAKCU in engine");
        self.stop_run();
        self.connect_makcu();
    }

    /// Lists the ports, shows the connecting state, and connects. Each connect starts here.
    pub(super) fn connect_makcu(&mut self) {
        let ports = output::ports();
        self.status.edit(|status| status.makcu.ports = ports);
        self.set_makcu_status(MakcuPhase::Connecting, None, None);
        self.status.flush();
        match self.output.connect() {
            Ok(device) => self.apply_makcu_connected(device),
            Err(error) => self.set_makcu_failure(format!("{error:#}")),
        }
    }

    /// Reports a lost link: it stops the run and shows a failure. The user connects again.
    pub(super) fn poll_makcu_link(&mut self) {
        if self.output.link_lost() {
            self.makcu_lost("MAKCU disconnected".to_owned());
        }
    }

    /// The link ended, by a failed write, a missing reply, or the check. Every such end comes here.
    pub(super) fn makcu_lost(&mut self, reason: String) {
        self.telemetry.event(TraceEvent::MakcuDisconnected);
        self.set_makcu_failure(reason);
    }

    /// Sends the stored Move interpolation. A failure of the value is shown on the error line, and
    /// the run still starts. A failure of the link is a lost link.
    pub(super) fn send_interpolation(&mut self) {
        let Err(error) = self.output.set_interpolation(self.tuning.interpolation) else {
            return;
        };
        let reason = format!("{error:#}");
        if !self.output.is_connected() {
            self.makcu_lost(reason);
            return;
        }
        tracing::warn!(%reason, "move interpolation failed");
        self.telemetry.event(TraceEvent::MakcuInterpolationFailed {
            reason: reason.clone(),
        });
        self.set_run_error(reason);
    }

    fn apply_makcu_connected(&mut self, device: Device) {
        tracing::info!(port = %device.port, firmware = device.firmware, "MAKCU connected in engine");
        self.telemetry.event(TraceEvent::MakcuConnected {
            port: device.port.clone(),
            firmware: device.firmware_label(),
            baud: Some(device.baud),
        });
        self.set_makcu_status(MakcuPhase::Connected, Some(device), None);
        // The device can lose the value when it restarts, so each connect sends it.
        self.send_interpolation();
    }

    pub(super) fn set_makcu_failure(&mut self, error: String) {
        tracing::error!(%error, "MAKCU operation failed");
        self.stop_run();
        self.set_makcu_status(MakcuPhase::Failed, None, Some(error));
    }

    /// The one place that edits the MAKCU status. Each phase sets the device and the error it has.
    fn set_makcu_status(
        &mut self,
        phase: MakcuPhase,
        device: Option<Device>,
        error: Option<String>,
    ) {
        self.status.edit(|status| {
            status.makcu.phase = phase;
            status.makcu.device = device;
            status.makcu.error = error;
        });
    }
}
