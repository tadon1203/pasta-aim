use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use makxd::{
    CommandApi, ConnectionState, ControllerState, Device, DeviceKind, DeviceKinds, MakxdError,
    crossbeam_channel::Receiver,
};

use crate::settings::InjectionApi;

const RECONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(25);
const RECONNECT_RETRY_WINDOW: Duration = Duration::from_millis(500);
const DISCONNECT_WAIT: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerKind {
    GenericHid,
    DualShock4,
    DualSense,
    DualSenseEdge,
    XboxGip,
    Xbox360,
}

impl ControllerKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::GenericHid => "Generic HID",
            Self::DualShock4 => "DualShock 4",
            Self::DualSense => "DualSense",
            Self::DualSenseEdge => "DualSense Edge",
            Self::XboxGip => "Xbox GIP",
            Self::Xbox360 => "Xbox 360 / XInput",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MakRoutes {
    pub mouse: bool,
    pub controllers: Vec<ControllerKind>,
}

impl MakRoutes {
    pub fn controller_available(&self) -> bool {
        !self.controllers.is_empty()
    }

    fn from_kinds(kinds: DeviceKinds) -> Self {
        let mut controllers = Vec::new();
        for (kind, label) in [
            (DeviceKind::GenericHid, ControllerKind::GenericHid),
            (DeviceKind::Ds4, ControllerKind::DualShock4),
            (DeviceKind::DualSenseDs5, ControllerKind::DualSense),
            (DeviceKind::DualSenseEdge, ControllerKind::DualSenseEdge),
            (DeviceKind::XboxGip, ControllerKind::XboxGip),
            (DeviceKind::Xbox360XInput, ControllerKind::Xbox360),
        ] {
            if kinds.has(kind) {
                controllers.push(label);
            }
        }
        Self {
            mouse: kinds.mouse(),
            controllers,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalControllerStatus {
    NotRouted,
    Checking,
    Connected,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct MakMetadata {
    pub port: String,
    pub firmware: u32,
    pub routes: MakRoutes,
    pub controller_release_pending: bool,
}

#[derive(Debug)]
pub enum MakConnectionEvent {
    Disconnected,
    Connecting,
    Connected(Result<MakMetadata, String>),
}

pub struct MakClient {
    device: Option<Device>,
    events: Option<Receiver<ConnectionState>>,
    routes: MakRoutes,
    port: Option<String>,
    firmware: Option<u32>,
    injection_api: InjectionApi,
    controller_release_pending: bool,
    physical_controller: PhysicalControllerStatus,
}

impl Default for MakClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MakClient {
    pub fn new() -> Self {
        Self {
            device: None,
            events: None,
            routes: MakRoutes::default(),
            port: None,
            firmware: None,
            injection_api: InjectionApi::default(),
            controller_release_pending: false,
            physical_controller: PhysicalControllerStatus::Unknown,
        }
    }

    pub fn connect_initial(&mut self) -> Result<MakMetadata> {
        tracing::info!("connecting to MAK device");
        self.connect_with_port(None, true)
    }

    pub fn refresh_routes(&mut self) -> Result<MakMetadata> {
        tracing::info!(preferred_port = ?self.port, "refreshing MAK routes");
        let preferred_port = self.port.clone();
        self.disconnect_for_reconnect()?;
        self.connect_after_shutdown(preferred_port.as_deref(), true)
    }

    pub fn rescan(&mut self) -> Result<MakMetadata> {
        tracing::info!("rescanning MAK devices");
        self.disconnect_for_reconnect()?;
        self.connect_after_shutdown(None, true)
    }

    pub fn poll_connection_event(&mut self) -> Option<MakConnectionEvent> {
        let event = self.events.as_ref()?.try_recv().ok()?;
        match event {
            ConnectionState::Disconnected => {
                tracing::warn!("MAK disconnected");
                self.routes = MakRoutes::default();
                self.physical_controller = PhysicalControllerStatus::Unknown;
                Some(MakConnectionEvent::Disconnected)
            }
            ConnectionState::Connecting => {
                tracing::info!("MAK reconnecting");
                self.routes = MakRoutes::default();
                self.physical_controller = PhysicalControllerStatus::Unknown;
                Some(MakConnectionEvent::Connecting)
            }
            ConnectionState::Connected => {
                tracing::info!("MAK connected; refreshing metadata");
                let metadata = self
                    .refresh_metadata()
                    .map_err(|error| format!("{error:#}"));
                Some(MakConnectionEvent::Connected(metadata))
            }
        }
    }

    pub fn mouse_available(&self) -> bool {
        self.routes.mouse
    }

    pub fn controller_available(&self) -> bool {
        self.routes.controller_available()
    }

    pub fn controller_release_pending(&self) -> bool {
        self.controller_release_pending
    }

    #[cfg(test)]
    fn injection_api(&self) -> InjectionApi {
        self.injection_api
    }

    pub fn set_injection_api(&mut self, api: InjectionApi) -> Result<()> {
        if self.injection_api == api {
            return Ok(());
        }
        if self.controller_release_pending {
            self.release_controller()?;
        }
        self.injection_api = api;
        tracing::info!(injection_api = ?self.injection_api, "MAK injection API changed");
        Ok(())
    }

    pub fn physical_controller_status(&self) -> PhysicalControllerStatus {
        self.physical_controller
    }

    pub fn is_connected(&self) -> bool {
        self.device.as_ref().is_some_and(Device::is_connected)
    }

    pub fn probe_physical_controller(&mut self) -> PhysicalControllerStatus {
        if !self.controller_available() {
            self.physical_controller = PhysicalControllerStatus::NotRouted;
            return self.physical_controller;
        }
        let Some(device) = self.device.as_ref() else {
            self.physical_controller = PhysicalControllerStatus::Unknown;
            return self.physical_controller;
        };
        if !device.is_connected() {
            self.physical_controller = PhysicalControllerStatus::Unknown;
            return self.physical_controller;
        }

        self.physical_controller = match device.controller_physical() {
            Ok(_) => PhysicalControllerStatus::Connected,
            Err(MakxdError::Disconnected) => PhysicalControllerStatus::Unknown,
            Err(_) => PhysicalControllerStatus::Unavailable,
        };
        tracing::debug!(status = ?self.physical_controller, "probed physical controller");
        self.physical_controller
    }

    pub fn move_mouse(&mut self, x: i32, y: i32) -> Result<()> {
        if self.controller_release_pending && self.controller_available() {
            self.release_controller()?;
        }
        if !self.mouse_available() {
            bail!("MAK reports no mouse route");
        }
        let x = x.clamp(i16::MIN as i32, i16::MAX as i32);
        let y = y.clamp(i16::MIN as i32, i16::MAX as i32);
        let started = Instant::now();
        self.connected_device()?
            .move_xy_with_api(sdk_command_api(self.injection_api), x, y)
            .context("MAK mouse move failed")?;
        if crate::logging::mak_command_trace_enabled() {
            tracing::trace!(
                x,
                y,
                injection_api = ?self.injection_api,
                elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
                "MAK mouse command completed"
            );
        }
        Ok(())
    }

    pub fn set_right_stick(&mut self, x: i32, y: i32) -> Result<()> {
        if !self.controller_available() {
            bail!("MAK reports no controller route");
        }
        let x = x.clamp(i16::MIN as i32, i16::MAX as i32);
        let y = y.clamp(i16::MIN as i32, i16::MAX as i32);
        let active = x != 0 || y != 0;
        let state = ControllerState {
            right_stick_x: x as i16,
            right_stick_y: y as i16,
            ..Default::default()
        };
        if active {
            // Treat a non-zero write as potentially accepted even if the transport reports
            // an error afterwards. The pending zero is the safe completion path on reconnect.
            self.controller_release_pending = true;
        }
        let started = Instant::now();
        self.connected_device()?
            .set_controller_state_with_api(sdk_command_api(self.injection_api), state)
            .context("MAK controller state write failed")?;
        if !active {
            self.controller_release_pending = false;
        }
        self.physical_controller = PhysicalControllerStatus::Connected;
        if crate::logging::mak_command_trace_enabled() {
            tracing::trace!(
                x,
                y,
                active,
                injection_api = ?self.injection_api,
                elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
                "MAK controller command completed"
            );
        }
        Ok(())
    }

    pub fn release_controller(&mut self) -> Result<()> {
        if !self.controller_release_pending {
            return Ok(());
        }
        if !self.controller_available() {
            return Ok(());
        }
        self.connected_device()?
            .set_controller_state_with_api(
                sdk_command_api(self.injection_api),
                ControllerState::default(),
            )
            .context("MAK controller release failed")?;
        self.controller_release_pending = false;
        Ok(())
    }

    pub fn release_controller_best_effort(&mut self) {
        if let Err(error) = self.release_controller() {
            tracing::warn!(error = %format!("{error:#}"), "best-effort controller release failed");
        }
    }

    fn connected_device(&self) -> Result<&Device> {
        let device = self.device.as_ref().context("MAK is not connected")?;
        if !device.is_connected() {
            bail!("MAK transport is not connected");
        }
        Ok(device)
    }

    fn connect_after_shutdown(
        &mut self,
        preferred_port: Option<&str>,
        allow_scan: bool,
    ) -> Result<MakMetadata> {
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
    ) -> Result<MakMetadata> {
        let device = if let Some(port) = preferred_port {
            match Device::connect_port(port) {
                Ok(device) => device,
                Err(port_error) if allow_scan => Device::connect().with_context(|| {
                    format!(
                        "failed to reconnect on {port} ({port_error}); automatic MAK scan also failed"
                    )
                })?,
                Err(error) => {
                    return Err(anyhow!(error)).with_context(|| format!("failed to connect to {port}"));
                }
            }
        } else {
            Device::connect().context("MAK device not found")?
        };
        self.accept_connected_device(device)
    }

    fn accept_connected_device(&mut self, device: Device) -> Result<MakMetadata> {
        self.events = Some(device.connection_events());
        self.device = Some(device);
        self.refresh_metadata()
    }

    fn refresh_metadata(&mut self) -> Result<MakMetadata> {
        let device = self.connected_device()?;
        let kinds = device
            .device()
            .context("failed to read MAK routed device kinds")?;
        let firmware = device
            .firmware_version()
            .context("failed to read MAK firmware version")?;
        let port = device.port_name();
        let routes = MakRoutes::from_kinds(kinds);

        self.routes = routes.clone();
        self.firmware = Some(firmware);
        self.port = Some(port.clone());
        self.physical_controller = if routes.controller_available() {
            PhysicalControllerStatus::Checking
        } else {
            PhysicalControllerStatus::NotRouted
        };
        tracing::info!(port = %port, firmware, mouse_route = routes.mouse, controller_routes = routes.controllers.len(), "MAK metadata refreshed");

        if self.controller_release_pending && routes.controller_available() {
            self.release_controller().context(
                "MAK reconnected but the pending controller zero-state handoff could not be sent",
            )?;
        }

        Ok(MakMetadata {
            port,
            firmware,
            routes,
            controller_release_pending: self.controller_release_pending,
        })
    }

    fn disconnect_for_reconnect(&mut self) -> Result<()> {
        tracing::debug!("disconnecting MAK for reconnect");
        let needs_drain = self.controller_release_pending;
        self.release_controller_best_effort();
        if needs_drain && let Some(device) = &self.device {
            let _ = device.drain();
        }
        if let Some(device) = &self.device {
            device
                .disconnect_wait(DISCONNECT_WAIT)
                .context("MAK transport did not finish teardown before reconnect")?;
        }
        self.events = None;
        self.device = None;
        self.routes = MakRoutes::default();
        self.firmware = None;
        self.physical_controller = PhysicalControllerStatus::Unknown;
        Ok(())
    }
}

fn sdk_command_api(api: InjectionApi) -> CommandApi {
    match api {
        InjectionApi::MakApi => CommandApi::Mak,
        InjectionApi::KmApi => CommandApi::Km,
    }
}

impl Drop for MakClient {
    fn drop(&mut self) {
        tracing::debug!("shutting down MAK session");
        let needs_drain = self.controller_release_pending;
        self.release_controller_best_effort();
        if needs_drain && let Some(device) = &self.device {
            let _ = device.drain();
        }
        if let Some(device) = &self.device {
            device.disconnect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_bits_are_reported_separately_from_physical_status() {
        let routes = MakRoutes::from_kinds(DeviceKinds {
            kinds: DeviceKind::Mouse as u8
                | DeviceKind::DualSenseDs5 as u8
                | DeviceKind::XboxGip as u8,
        });

        assert!(routes.mouse);
        assert_eq!(
            routes.controllers,
            vec![ControllerKind::DualSense, ControllerKind::XboxGip]
        );
    }

    #[test]
    fn injection_api_switch_releases_controller_using_old_api_first() {
        let (device, mock) = Device::mock();
        let mut client = MakClient::new();
        client.device = Some(device);
        client.routes.controllers.push(ControllerKind::GenericHid);
        client.controller_release_pending = true;

        client.set_injection_api(InjectionApi::KmApi).unwrap();

        assert_eq!(client.injection_api(), InjectionApi::KmApi);
        assert!(!client.controller_release_pending());
        let commands = mock.sent_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(&commands[0][..5], &[0xde, 0xad, 20, 0, 0x40]);
        assert_eq!(&commands[0][5..], &[0; 20]);
    }

    #[test]
    fn failed_injection_api_release_keeps_old_api() {
        let mut client = MakClient::new();
        client.routes.controllers.push(ControllerKind::GenericHid);
        client.controller_release_pending = true;

        assert!(client.set_injection_api(InjectionApi::KmApi).is_err());
        assert_eq!(client.injection_api(), InjectionApi::MakApi);
    }
}
