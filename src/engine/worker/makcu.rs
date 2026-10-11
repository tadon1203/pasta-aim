//! MAKCU connection lifecycle.

use anyhow::Result;

use super::Worker;
use crate::{
    engine::MakcuPhase,
    makcu::{MakcuClient, MakcuConnectionEvent, MakcuMetadata, MakcuRoutes},
};

impl Worker {
    pub(super) fn connect_makcu_initial(&mut self) {
        tracing::info!("connecting MAKCU during engine startup");
        self.status.edit(|status| {
            status.makcu.phase = MakcuPhase::Connecting;
            status.makcu.error = None;
        });
        self.status.flush();
        let result = self.makcu.connect_initial();
        self.apply_makcu_result(result);
    }

    /// Drops the current connection state and reconnects with `connect`.
    pub(super) fn reconnect_makcu(
        &mut self,
        action: &str,
        connect: fn(&mut MakcuClient) -> Result<MakcuMetadata>,
    ) {
        tracing::info!(action, "reconnecting MAKCU in engine");
        self.stop_run();
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
        self.status.edit(|status| {
            status.makcu.phase = MakcuPhase::Connected;
            status.makcu.port = Some(metadata.port);
            status.makcu.firmware = Some(metadata.firmware);
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
