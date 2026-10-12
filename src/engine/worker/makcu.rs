//! MAKCU connection lifecycle.

use anyhow::Result;

use super::Worker;
use crate::{
    engine::MakcuPhase,
    makcu::{MakcuClient, MakcuConnectionEvent, MakcuMetadata, MakcuRoutes, format_firmware},
    telemetry::TraceEvent,
};

impl Worker {
    pub(super) fn connect_makcu_initial(&mut self) {
        tracing::info!("connecting MAKCU during engine startup");
        self.status.edit(|status| {
            status.makcu.phase = MakcuPhase::Connecting;
            status.makcu.error = None;
        });
        self.refresh_makcu_ports();
        self.status.flush();
        let result = self.makcu.connect_initial();
        self.apply_makcu_result(result);
    }

    /// Lists the serial ports for the port selector. A failure leaves the list empty, which only
    /// hides the choices: the connect does not need it.
    fn refresh_makcu_ports(&mut self) {
        let ports = MakcuClient::available_ports().unwrap_or_else(|error| {
            tracing::warn!(error = %crate::logging::chain(&error), "serial ports could not be listed");
            Vec::new()
        });
        self.status.edit(|status| status.makcu.ports = ports);
    }

    /// Drops the current connection state and reconnects with `connect`.
    pub(super) fn reconnect_makcu(
        &mut self,
        action: &str,
        connect: fn(&mut MakcuClient) -> Result<MakcuMetadata>,
    ) {
        tracing::info!(action, "reconnecting MAKCU in engine");
        self.stop_run();
        self.refresh_makcu_ports();
        self.status.edit(|status| {
            status.makcu.phase = MakcuPhase::Connecting;
            status.makcu.routes = MakcuRoutes::default();
            status.makcu.error = None;
        });
        self.status.flush();
        let result = connect(&mut self.makcu);
        self.apply_makcu_result(result);
    }

    fn apply_makcu_result(&mut self, result: Result<MakcuMetadata>) {
        match result {
            Ok(metadata) => self.apply_makcu_metadata(metadata),
            Err(error) => self.set_makcu_failure(format!("{error:#}")),
        }
    }

    pub(super) fn poll_makcu_events(&mut self) {
        loop {
            let event = self.makcu.poll_connection_event();
            let Some(event) = event else { break };
            match event {
                MakcuConnectionEvent::Disconnected | MakcuConnectionEvent::Connecting => {
                    tracing::warn!(?event, "MAKCU connection state changed");
                    if matches!(event, MakcuConnectionEvent::Disconnected) {
                        self.telemetry.event(TraceEvent::MakcuDisconnected);
                    }
                    self.stop_run();
                    self.status.edit(|status| {
                        status.makcu.phase = MakcuPhase::Reconnecting;
                        status.makcu.routes = MakcuRoutes::default();
                        status.makcu.error = None;
                    });
                }
                MakcuConnectionEvent::Connected(Ok(metadata)) => {
                    self.apply_makcu_metadata(metadata)
                }
                MakcuConnectionEvent::Connected(Err(error)) => self.set_makcu_failure(error),
            }
        }
    }

    fn apply_makcu_metadata(&mut self, metadata: MakcuMetadata) {
        tracing::info!(port = %metadata.port, firmware = metadata.firmware, "MAKCU connected in engine");
        self.telemetry.event(TraceEvent::MakcuConnected {
            port: metadata.port.clone(),
            firmware: format_firmware(metadata.firmware),
            baud: Some(metadata.baud),
        });
        // The Run still starts: a stored interpolation that the device did not confirm is only reported.
        if let Some(error) = metadata.interpolation_error {
            tracing::warn!(%error, "move interpolation was not applied on connect");
            self.telemetry.event(TraceEvent::MakcuInterpolationFailed {
                reason: error.clone(),
            });
            self.set_run_error(error);
        }
        self.status.edit(|status| {
            status.makcu.phase = MakcuPhase::Connected;
            status.makcu.port = Some(metadata.port);
            status.makcu.firmware = Some(metadata.firmware);
            status.makcu.baud = Some(metadata.baud);
            status.makcu.routes = metadata.routes;
            status.makcu.error = None;
        });
    }

    fn set_makcu_failure(&mut self, error: String) {
        tracing::error!(%error, "MAKCU operation failed");
        self.stop_run();
        self.status.edit(|status| {
            status.makcu.phase = MakcuPhase::Failed;
            status.makcu.routes = MakcuRoutes::default();
            status.makcu.error = Some(error);
        });
    }
}
