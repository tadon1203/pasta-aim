use super::{OutputPhase, Worker};
use crate::output::{self, Device};

impl Worker {
    pub(super) fn connect_output(&mut self) {
        let ports = output::ports();
        self.edit(|state| state.output.ports = ports);
        self.set_output_state(OutputPhase::Connecting, None, None);
        self.publish();
        match self.output.connect() {
            Ok(device) => {
                tracing::info!(port = %device.port, firmware = device.firmware, "MAKCU connected in engine");
                self.set_output_state(OutputPhase::Connected, Some(device), None);
                // The device can lose the value when it restarts, so each connect sends it.
                self.send_interpolation();
            }
            Err(error) => self.output_lost(format!("{error:#}")),
        }
    }

    pub(super) fn poll_output_link(&mut self) {
        if self.output.link_lost() {
            self.output_lost("MAKCU disconnected".to_owned());
        }
    }

    pub(super) fn output_lost(&mut self, error: String) {
        tracing::error!(%error, "MAKCU operation failed");
        self.stop_run();
        self.set_output_state(OutputPhase::Failed, None, Some(error));
    }

    pub(super) fn send_interpolation(&mut self) {
        let Err(error) = self.output.set_interpolation(self.settings.interpolation) else {
            return;
        };
        let reason = format!("{error:#}");
        if !self.output.is_connected() {
            self.output_lost(reason);
            return;
        }
        tracing::warn!(%reason, "move interpolation failed");
        self.set_run_error(reason);
    }

    fn set_output_state(
        &mut self,
        phase: OutputPhase,
        device: Option<Device>,
        error: Option<String>,
    ) {
        self.edit(|state| {
            state.output.phase = phase;
            state.output.device = device;
            state.output.error = error;
        });
    }
}
