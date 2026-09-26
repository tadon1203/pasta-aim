//! MAK connection lifecycle and physical-controller probing.

use std::time::Instant;

use anyhow::Result;

use super::{PHYSICAL_CONTROLLER_POLL_INTERVAL, Worker};
use crate::{
    engine::MakPhase,
    mak::{MakClient, MakConnectionEvent, MakMetadata, MakRoutes, PhysicalControllerStatus},
    settings::OutputMode,
};

impl Worker {
    pub(super) fn connect_mak_initial(&mut self) {
        tracing::info!("connecting MAK during engine startup");
        self.status.edit(|status| {
            status.mak.phase = MakPhase::Connecting;
            status.mak.error = None;
        });
        self.status.flush();
        let result = self.mak().connect_initial();
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
            status.mak.physical_controller = PhysicalControllerStatus::Unknown;
            status.mak.error = None;
        });
        self.status.flush();
        let result = connect(&mut self.mak());
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
            let event = self.mak().poll_connection_event();
            let Some(event) = event else { break };
            match event {
                MakConnectionEvent::Disconnected | MakConnectionEvent::Connecting => {
                    tracing::warn!(?event, "MAK connection state changed");
                    self.stop_run();
                    self.status.edit(|status| {
                        status.mak.phase = MakPhase::Reconnecting;
                        status.mak.routes = MakRoutes::default();
                        status.mak.physical_controller = PhysicalControllerStatus::Unknown;
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
        let physical = if metadata.routes.controller_available() {
            PhysicalControllerStatus::Checking
        } else {
            PhysicalControllerStatus::NotRouted
        };
        self.last_physical_probe = Instant::now() - PHYSICAL_CONTROLLER_POLL_INTERVAL;
        self.status.edit(|status| {
            status.mak.phase = MakPhase::Connected;
            status.mak.port = Some(metadata.port);
            status.mak.firmware = Some(metadata.firmware);
            status.mak.routes = metadata.routes;
            status.mak.physical_controller = physical;
            status.mak.controller_release_pending = metadata.controller_release_pending;
            status.mak.error = None;
        });
    }

    fn set_mak_failure(&mut self, error: String) {
        tracing::error!(%error, "MAK operation failed");
        self.stop_run();
        self.status.edit(|status| {
            status.mak.phase = MakPhase::Failed;
            status.mak.routes = MakRoutes::default();
            status.mak.physical_controller = PhysicalControllerStatus::Unknown;
            status.mak.error = Some(error);
        });
    }

    pub(super) fn sync_release_pending_status(&mut self) {
        let pending = self.mak().controller_release_pending();
        self.status
            .edit(|status| status.mak.controller_release_pending = pending);
    }

    pub(super) fn maybe_probe_physical_controller(&mut self) {
        if self.last_physical_probe.elapsed() < PHYSICAL_CONTROLLER_POLL_INTERVAL {
            return;
        }
        let should_probe = {
            let mak = self.mak();
            mak.is_connected()
                && mak.controller_available()
                && (self.tuning.output.mode == OutputMode::Controller
                    || mak.physical_controller_status() == PhysicalControllerStatus::Checking)
        };
        if should_probe {
            self.probe_physical_controller();
        }
    }

    pub(super) fn probe_physical_controller(&mut self) {
        if self.run.is_continuous() || !self.mak().is_connected() {
            return;
        }
        self.last_physical_probe = Instant::now();
        self.status.edit(|status| {
            if status.mak.routes.controller_available() {
                status.mak.physical_controller = PhysicalControllerStatus::Checking;
            }
        });
        let physical = self.mak().probe_physical_controller();
        tracing::debug!(?physical, "physical controller probe completed");
        self.status
            .edit(|status| status.mak.physical_controller = physical);
    }
}
