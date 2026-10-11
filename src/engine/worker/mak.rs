//! MAK connection lifecycle.

use anyhow::Result;

use super::Worker;
use crate::{
    engine::MakPhase,
    mak::{MakClient, MakConnectionEvent, MakMetadata, MakRoutes},
};

impl Worker {
    pub(super) fn connect_mak_initial(&mut self) {
        tracing::info!("connecting MAK during engine startup");
        self.status.edit(|status| {
            status.mak.phase = MakPhase::Connecting;
            status.mak.error = None;
        });
        self.status.flush();
        let result = self.mak.connect_initial();
        self.apply_mak_result(result);
    }

    /// Drops the current connection state and reconnects with `connect`.
    pub(super) fn reconnect_mak(
        &mut self,
        action: &str,
        connect: fn(&mut MakClient) -> Result<MakMetadata>,
    ) {
        tracing::info!(action, "reconnecting MAK in engine");
        self.stop_run();
        self.status.edit(|status| {
            status.mak.phase = MakPhase::Connecting;
            status.mak.routes = MakRoutes::default();
            status.mak.error = None;
        });
        self.status.flush();
        let result = connect(&mut self.mak);
        self.apply_mak_result(result);
    }

    fn apply_mak_result(&mut self, result: Result<MakMetadata>) {
        match result {
            Ok(metadata) => self.apply_mak_metadata(metadata),
            Err(error) => self.set_mak_failure(format!("{error:#}")),
        }
    }

    pub(super) fn poll_mak_events(&mut self) {
        loop {
            let event = self.mak.poll_connection_event();
            let Some(event) = event else { break };
            match event {
                MakConnectionEvent::Disconnected | MakConnectionEvent::Connecting => {
                    tracing::warn!(?event, "MAK connection state changed");
                    self.stop_run();
                    self.status.edit(|status| {
                        status.mak.phase = MakPhase::Reconnecting;
                        status.mak.routes = MakRoutes::default();
                        status.mak.error = None;
                    });
                }
                MakConnectionEvent::Connected(Ok(metadata)) => self.apply_mak_metadata(metadata),
                MakConnectionEvent::Connected(Err(error)) => self.set_mak_failure(error),
            }
        }
    }

    fn apply_mak_metadata(&mut self, metadata: MakMetadata) {
        tracing::info!(port = %metadata.port, firmware = metadata.firmware, "MAK connected in engine");
        self.status.edit(|status| {
            status.mak.phase = MakPhase::Connected;
            status.mak.port = Some(metadata.port);
            status.mak.firmware = Some(metadata.firmware);
            status.mak.routes = metadata.routes;
            status.mak.error = None;
        });
    }

    fn set_mak_failure(&mut self, error: String) {
        tracing::error!(%error, "MAK operation failed");
        self.stop_run();
        self.status.edit(|status| {
            status.mak.phase = MakPhase::Failed;
            status.mak.routes = MakRoutes::default();
            status.mak.error = Some(error);
        });
    }
}
