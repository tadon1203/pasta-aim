use std::{thread, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use makxd::{
    ConnectionState, ControllerState, Device, DeviceKind, DeviceKinds, MakxdError,
    crossbeam_channel::Receiver,
};

use crate::config::OutputMode;

const RECONNECT_SETTLE: Duration = Duration::from_millis(250);

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
}

#[derive(Debug)]
pub enum MakConnectionEvent {
    Disconnected,
    Connecting,
    Connected(Result<MakMetadata, String>),
}

pub struct MakOutput {
    device: Option<Device>,
    events: Option<Receiver<ConnectionState>>,
    routes: MakRoutes,
    port: Option<String>,
    firmware: Option<u32>,
    controller_engaged: bool,
    physical_controller: PhysicalControllerStatus,
}

impl Default for MakOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl MakOutput {
    pub fn new() -> Self {
        Self {
            device: None,
            events: None,
            routes: MakRoutes::default(),
            port: None,
            firmware: None,
            controller_engaged: false,
            physical_controller: PhysicalControllerStatus::Unknown,
        }
    }

    pub fn connect_initial(&mut self) -> Result<MakMetadata> {
        self.connect_with_port(None, true)
    }

    pub fn refresh_routes(&mut self) -> Result<MakMetadata> {
        let preferred_port = self.port.clone();
        self.disconnect_for_reconnect();
        self.connect_with_port(preferred_port.as_deref(), true)
    }

    pub fn rescan(&mut self) -> Result<MakMetadata> {
        self.disconnect_for_reconnect();
        self.connect_with_port(None, true)
    }

    pub fn poll_connection_event(&mut self) -> Option<MakConnectionEvent> {
        let event = self.events.as_ref()?.try_recv().ok()?;
        match event {
            ConnectionState::Disconnected => {
                self.controller_engaged = false;
                self.routes = MakRoutes::default();
                self.physical_controller = PhysicalControllerStatus::Unknown;
                Some(MakConnectionEvent::Disconnected)
            }
            ConnectionState::Connecting => {
                self.routes = MakRoutes::default();
                self.physical_controller = PhysicalControllerStatus::Unknown;
                Some(MakConnectionEvent::Connecting)
            }
            ConnectionState::Connected => {
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
        self.physical_controller
    }

    pub fn send(&mut self, mode: OutputMode, x: i32, y: i32) -> Result<()> {
        let x = x.clamp(i16::MIN as i32, i16::MAX as i32);
        let y = y.clamp(i16::MIN as i32, i16::MAX as i32);

        match mode {
            OutputMode::Mouse => {
                self.release_controller()?;
                if !self.mouse_available() {
                    bail!("MAK reports no mouse route");
                }
                self.connected_device()?
                    .move_xy(x, y)
                    .context("MAK mouse move failed")?;
            }
            OutputMode::Controller => {
                if !self.controller_available() {
                    bail!("MAK reports no controller route");
                }
                let state = ControllerState {
                    right_stick_x: x as i16,
                    right_stick_y: y as i16,
                    ..Default::default()
                };
                self.connected_device()?
                    .set_controller_state(state)
                    .context("MAK controller state write failed")?;
                self.controller_engaged = true;
                self.physical_controller = PhysicalControllerStatus::Connected;
            }
        }
        Ok(())
    }

    pub fn release_controller(&mut self) -> Result<()> {
        if !self.controller_engaged {
            return Ok(());
        }
        self.connected_device()?
            .set_controller_state(ControllerState::default())
            .context("MAK controller release failed")?;
        self.controller_engaged = false;
        Ok(())
    }

    pub fn release_controller_best_effort(&mut self) {
        let _ = self.release_controller();
        self.controller_engaged = false;
    }

    fn connected_device(&self) -> Result<&Device> {
        let device = self.device.as_ref().context("MAK is not connected")?;
        if !device.is_connected() {
            bail!("MAK transport is not connected");
        }
        Ok(device)
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

        Ok(MakMetadata {
            port,
            firmware,
            routes,
        })
    }

    fn disconnect_for_reconnect(&mut self) {
        self.release_controller_best_effort();
        if let Some(device) = &self.device {
            device.disconnect();
        }
        self.events = None;
        self.device = None;
        self.routes = MakRoutes::default();
        self.firmware = None;
        self.physical_controller = PhysicalControllerStatus::Unknown;

        // The SDK reader exits after its bounded serial read timeout. Waiting
        // past that bound prevents a manual refresh from racing the old COM handle.
        thread::sleep(RECONNECT_SETTLE);
    }
}

impl Drop for MakOutput {
    fn drop(&mut self) {
        self.release_controller_best_effort();
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
}
