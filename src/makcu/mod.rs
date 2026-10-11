use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use makxd::{CommandApi, ConnectionState, Device, DeviceKinds, crossbeam_channel::Receiver};

use crate::settings::InjectionApi;

#[expect(
    dead_code,
    reason = "nothing outside src/makcu calls the port until ticket 04"
)]
mod port;
#[expect(
    dead_code,
    reason = "nothing outside src/makcu calls the protocol until ticket 04"
)]
mod protocol;
#[expect(
    dead_code,
    reason = "nothing outside src/makcu calls the session until ticket 04"
)]
mod session;

const RECONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(25);
const RECONNECT_RETRY_WINDOW: Duration = Duration::from_millis(500);
const DISCONNECT_WAIT: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Default)]
pub struct MakcuRoutes {
    pub mouse: bool,
}

impl MakcuRoutes {
    fn from_kinds(kinds: DeviceKinds) -> Self {
        Self {
            mouse: kinds.mouse(),
        }
    }
}

/// Counters of the Raw moves. See `docs/makcu-protocol.md`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawStats {
    pub submitted: u64,
    pub rejected: u64,
    pub timed_out: u64,
    pub consecutive_rejected: u32,
    pub last_reason: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct MakcuMetadata {
    pub port: String,
    pub firmware: u32,
    pub routes: MakcuRoutes,
}

#[derive(Debug)]
pub enum MakcuConnectionEvent {
    Disconnected,
    Connecting,
    Connected(Result<MakcuMetadata, String>),
}

pub struct MakcuClient {
    device: Option<Device>,
    events: Option<Receiver<ConnectionState>>,
    routes: MakcuRoutes,
    port: Option<String>,
    firmware: Option<u32>,
    injection_api: InjectionApi,
}

impl Default for MakcuClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MakcuClient {
    #[cfg(test)]
    pub(crate) fn mock() -> (Self, std::sync::Arc<makxd::MockTransport>) {
        let (device, transport) = Device::mock();
        let mut client = Self::new();
        client.device = Some(device);
        client.routes = MakcuRoutes { mouse: true };
        (client, transport)
    }

    pub fn new() -> Self {
        Self {
            device: None,
            events: None,
            routes: MakcuRoutes::default(),
            port: None,
            firmware: None,
            injection_api: InjectionApi::default(),
        }
    }

    pub fn connect_initial(&mut self) -> Result<MakcuMetadata> {
        tracing::info!("connecting to MAKCU device");
        self.connect_with_port(None, true)
    }

    pub fn refresh_routes(&mut self) -> Result<MakcuMetadata> {
        tracing::info!(preferred_port = ?self.port, "refreshing MAKCU routes");
        let preferred_port = self.port.clone();
        self.disconnect_for_reconnect()?;
        self.connect_after_shutdown(preferred_port.as_deref(), true)
    }

    pub fn rescan(&mut self) -> Result<MakcuMetadata> {
        tracing::info!("rescanning MAKCU devices");
        self.disconnect_for_reconnect()?;
        self.connect_after_shutdown(None, true)
    }

    pub fn poll_connection_event(&mut self) -> Option<MakcuConnectionEvent> {
        let event = self.events.as_ref()?.try_recv().ok()?;
        match event {
            ConnectionState::Disconnected => {
                tracing::warn!("MAKCU disconnected");
                self.routes = MakcuRoutes::default();
                Some(MakcuConnectionEvent::Disconnected)
            }
            ConnectionState::Connecting => {
                tracing::info!("MAKCU reconnecting");
                self.routes = MakcuRoutes::default();
                Some(MakcuConnectionEvent::Connecting)
            }
            ConnectionState::Connected => {
                tracing::info!("MAKCU connected; refreshing metadata");
                let metadata = self
                    .refresh_metadata()
                    .map_err(|error| format!("{error:#}"));
                Some(MakcuConnectionEvent::Connected(metadata))
            }
        }
    }

    pub fn mouse_available(&self) -> bool {
        self.routes.mouse
    }

    #[cfg(test)]
    fn injection_api(&self) -> InjectionApi {
        self.injection_api
    }

    pub fn set_injection_api(&mut self, api: InjectionApi) -> Result<()> {
        if self.injection_api == api {
            return Ok(());
        }
        self.injection_api = api;
        tracing::info!(injection_api = ?self.injection_api, "MAKCU injection API changed");
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.device.as_ref().is_some_and(Device::is_connected)
    }

    pub fn move_mouse(&mut self, x: i32, y: i32) -> Result<()> {
        if !self.mouse_available() {
            bail!("MAKCU reports no mouse route");
        }
        let x = x.clamp(i16::MIN as i32, i16::MAX as i32);
        let y = y.clamp(i16::MIN as i32, i16::MAX as i32);
        let started = Instant::now();
        self.connected_device()?
            .move_xy_with_api(sdk_command_api(self.injection_api), x, y)
            .context("MAKCU mouse move failed")?;
        if crate::logging::makcu_command_trace_enabled() {
            tracing::trace!(
                x,
                y,
                injection_api = ?self.injection_api,
                elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
                "MAKCU mouse command completed"
            );
        }
        Ok(())
    }

    fn connected_device(&self) -> Result<&Device> {
        let device = self.device.as_ref().context("MAKCU is not connected")?;
        if !device.is_connected() {
            bail!("MAKCU transport is not connected");
        }
        Ok(device)
    }

    fn connect_after_shutdown(
        &mut self,
        preferred_port: Option<&str>,
        allow_scan: bool,
    ) -> Result<MakcuMetadata> {
        if let Some(port) = preferred_port {
            let started = Instant::now();
            let mut last_error = None;
            while started.elapsed() < RECONNECT_RETRY_WINDOW {
                match Device::connect_port(port) {
                    Ok(device) => return self.accept_connected_device(device),
                    Err(error) => {
                        last_error = Some(error.to_string());
                        thread::sleep(RECONNECT_RETRY_INTERVAL);
                    }
                }
            }

            if !allow_scan {
                bail!(
                    "failed to reconnect on {port}: {}",
                    last_error.unwrap_or_else(|| "unknown error".to_owned())
                );
            }
        }

        self.connect_with_port(None, allow_scan)
    }

    fn connect_with_port(
        &mut self,
        preferred_port: Option<&str>,
        allow_scan: bool,
    ) -> Result<MakcuMetadata> {
        let device = if let Some(port) = preferred_port {
            match Device::connect_port(port) {
                Ok(device) => device,
                Err(port_error) if allow_scan => Device::connect().with_context(|| {
                    format!(
                        "failed to reconnect on {port} ({port_error}); automatic MAKCU scan also failed"
                    )
                })?,
                Err(error) => {
                    return Err(anyhow!(error)).with_context(|| format!("failed to connect to {port}"));
                }
            }
        } else {
            Device::connect().context("MAKCU device not found")?
        };
        self.accept_connected_device(device)
    }

    fn accept_connected_device(&mut self, device: Device) -> Result<MakcuMetadata> {
        self.events = Some(device.connection_events());
        self.device = Some(device);
        self.refresh_metadata()
    }

    fn refresh_metadata(&mut self) -> Result<MakcuMetadata> {
        let device = self.connected_device()?;
        let kinds = device
            .device()
            .context("failed to read MAKCU routed device kinds")?;
        let firmware = device
            .firmware_version()
            .context("failed to read MAKCU firmware version")?;
        let port = device.port_name();
        let routes = MakcuRoutes::from_kinds(kinds);

        self.routes = routes.clone();
        self.firmware = Some(firmware);
        self.port = Some(port.clone());
        tracing::info!(port = %port, firmware, mouse_route = routes.mouse, "MAKCU metadata refreshed");

        Ok(MakcuMetadata {
            port,
            firmware,
            routes,
        })
    }

    fn disconnect_for_reconnect(&mut self) -> Result<()> {
        tracing::debug!("disconnecting MAKCU for reconnect");
        if let Some(device) = &self.device {
            device
                .disconnect_wait(DISCONNECT_WAIT)
                .context("MAKCU transport did not finish teardown before reconnect")?;
        }
        self.events = None;
        self.device = None;
        self.routes = MakcuRoutes::default();
        self.firmware = None;
        Ok(())
    }
}

fn sdk_command_api(api: InjectionApi) -> CommandApi {
    match api {
        InjectionApi::MakApi => CommandApi::Mak,
        InjectionApi::KmApi => CommandApi::Km,
    }
}

impl Drop for MakcuClient {
    fn drop(&mut self) {
        tracing::debug!("shutting down MAKCU session");
        if let Some(device) = &self.device {
            device.disconnect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makxd::DeviceKind;

    #[test]
    fn only_the_mouse_route_is_reported() {
        let routes = MakcuRoutes::from_kinds(DeviceKinds {
            kinds: DeviceKind::Mouse as u8 | DeviceKind::DualSenseDs5 as u8,
        });
        assert!(routes.mouse);

        let routes = MakcuRoutes::from_kinds(DeviceKinds {
            kinds: DeviceKind::XboxGip as u8,
        });
        assert!(!routes.mouse);
    }

    #[test]
    fn injection_api_switch_takes_effect() {
        let mut client = MakcuClient::new();
        client.set_injection_api(InjectionApi::KmApi).unwrap();
        assert_eq!(client.injection_api(), InjectionApi::KmApi);
    }
}
