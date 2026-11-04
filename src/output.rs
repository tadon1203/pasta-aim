//! The output: it writes the mouse move to MAKCU.
//!
//! The engine owns one `Output` and calls it on the engine thread. There is no I/O thread: a move
//! is one write, and the one query with a reply waits for it. The port is in `port`, the connect
//! in `connect`, and the wire format in `protocol`.

mod connect;
#[cfg(test)]
pub(crate) mod fake;
mod port;
mod protocol;

use std::{fmt, sync::Arc};

use anyhow::{Context, Result, ensure};

use self::{
    connect::{exchange, write_queries},
    port::{Connector, Serial, WindowsConnector},
    protocol::{Query, confirms_interpolate},
};
use crate::{aim::Move, settings::Interpolation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub port: String,
    pub firmware: u32,
    pub baud: u32,
    pub mouse_route: bool,
}

impl Device {
    /// The firmware as the UI and the trace show it, such as `V4.094`.
    pub fn firmware_label(&self) -> String {
        protocol::format_firmware(self.firmware)
    }
}

/// A serial port of the system, as the port selector lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    pub name: String,
    /// The label that the selector shows after the name: `MAKCU` for a MAKCU chip, empty otherwise.
    pub description: String,
}

/// Every serial port of the system, for the port selector. A failure leaves the list empty, which
/// only hides the choices: the connect does not need it.
pub fn ports() -> Vec<Port> {
    WindowsConnector.ports().unwrap_or_else(|error| {
        tracing::warn!(%error, "serial ports could not be listed");
        Vec::new()
    })
}

/// An open port with the device behind it. A failed write or a missing reply ends the link for
/// good: the engine connects a new one.
struct Link {
    port: Box<dyn Serial>,
    device: Device,
    /// The buffer that a write encodes into, kept for the next write.
    buffer: Vec<u8>,
}

pub struct Output {
    connector: Arc<dyn Connector>,
    /// `None` while disconnected.
    link: Option<Link>,
    /// The port that the user chose. `None` is auto-detect.
    chosen: Option<String>,
}

impl fmt::Debug for Output {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Output").finish_non_exhaustive()
    }
}

impl Default for Output {
    fn default() -> Self {
        Self::with_connector(Arc::new(WindowsConnector))
    }
}

impl Output {
    fn with_connector(connector: Arc<dyn Connector>) -> Self {
        Self {
            connector,
            link: None,
            chosen: None,
        }
    }

    /// Chooses the port: `None` is auto. A chosen port is the only one tried, also on reconnects.
    pub fn set_port(&mut self, port: Option<String>) {
        self.chosen = port;
    }

    /// Closes the link and connects. The Move interpolation is not sent here: the caller sends it.
    pub fn connect(&mut self) -> Result<Device> {
        tracing::info!(chosen_port = ?self.chosen, "connecting to MAKCU device");
        // The old link closes its port before the new connect.
        self.link = None;
        let (port, device) = connect::connect(self.connector.as_ref(), self.chosen.as_deref())?;
        tracing::info!(
            port = %device.port,
            firmware = device.firmware,
            baud = device.baud,
            mouse_route = device.mouse_route,
            "MAKCU connected"
        );
        self.link = Some(Link {
            port,
            device: device.clone(),
            buffer: Vec::new(),
        });
        Ok(device)
    }

    /// Whether the device went away since the last call. The link is dropped then, and the caller
    /// connects again. A link that a failed write dropped is not reported: that write was the report.
    pub fn link_lost(&mut self) -> bool {
        let Some(link) = self.link.as_mut() else {
            return false;
        };
        if link.port.check().is_ok() {
            return false;
        }
        tracing::warn!("MAKCU disconnected");
        self.link = None;
        true
    }

    pub fn is_connected(&self) -> bool {
        self.link.is_some()
    }

    pub fn mouse_route(&self) -> bool {
        self.link
            .as_ref()
            .is_some_and(|link| link.device.mouse_route)
    }

    /// Sends the Move interpolation and checks the echo. The device can lose the value when it
    /// restarts, so the caller sends it after each connect.
    /// A reply that does not confirm the value is an error, and the link stays up. A missing reply
    /// ends the link: a late reply would look like the answer to the next query.
    pub fn set_interpolation(&mut self, interpolation: Interpolation) -> Result<()> {
        let value = interpolation.wire_value();
        let link = self.link.as_mut().context("MAKCU is not connected")?;
        let query = Query::Interpolate(value);
        let packet = match exchange(link.port.as_mut(), query) {
            Ok(packet) => packet,
            Err(error) => {
                self.link = None;
                return Err(error.context("MAKCU did not confirm the move interpolation"));
            }
        };
        ensure!(
            confirms_interpolate(value, &packet),
            "MAKCU answered the move interpolation with {:?}",
            packet.payload()
        );
        Ok(())
    }

    /// Writes one move as one port write. It never waits for a reply. A failed write ends the link.
    pub fn write(&mut self, step: Move) -> Result<()> {
        let link = self.link.as_mut().context("MAKCU is not connected")?;
        let query = Query::Move {
            x: step.x,
            y: step.y,
        };
        if let Err(error) = write_queries(link.port.as_mut(), &mut link.buffer, &[query]) {
            self.link = None;
            return Err(error).context("MAKCU mouse move failed");
        }
        Ok(())
    }
}

#[cfg(test)]
impl Output {
    /// A connected output on a fake MAKCU, which the test can change and inspect.
    pub(crate) fn fake() -> (Self, fake::FakeMakcu) {
        let fake = fake::FakeMakcu::new();
        let mut output = Self::with_connector(Arc::new(fake.clone()));
        output.connect().expect("the fake MAKCU connects");
        (output, fake)
    }
}

#[cfg(test)]
mod tests {
    use super::{protocol::tests::frame, *};
    use crate::settings::InterpolationPercent;

    fn movement(x: i16, y: i16) -> Move {
        Move { x, y }
    }

    fn percent(value: u16) -> Interpolation {
        Interpolation::Percent(InterpolationPercent::new(value))
    }

    /// The last query that the fake received.
    fn last_frame(fake: &fake::FakeMakcu) -> Vec<u8> {
        fake.frames().last().unwrap().clone()
    }

    #[test]
    fn a_move_is_one_write_of_the_move_request() {
        let (mut output, fake) = Output::fake();
        output.write(movement(20, -3)).unwrap();
        assert_eq!(last_frame(&fake), frame(Query::Move { x: 20, y: -3 }));
    }

    #[test]
    fn a_move_without_a_link_is_an_error() {
        let mut output = Output::with_connector(Arc::new(fake::FakeMakcu::new()));
        let error = output.write(movement(1, 1)).unwrap_err();
        assert_eq!(error.to_string(), "MAKCU is not connected");
    }

    #[test]
    fn a_failed_write_ends_the_link_and_is_not_reported_again() {
        let (mut output, fake) = Output::fake();
        fake.disconnect();
        let error = output.write(movement(1, 1)).unwrap_err();
        assert_eq!(error.to_string(), "MAKCU mouse move failed");
        assert!(!output.is_connected());
        assert!(!output.link_lost(), "the failed write was the report");
    }

    #[test]
    fn a_lost_link_is_reported_once_and_a_reconnect_works() {
        let (mut output, fake) = Output::fake();
        assert!(!output.link_lost());
        fake.disconnect();
        assert!(output.link_lost());
        assert!(!output.link_lost(), "the loss is reported once");
        assert!(!output.is_connected());
        fake.reconnect();
        output.connect().unwrap();
        assert!(output.is_connected());
    }

    #[test]
    fn the_connect_returns_the_device_and_the_output_reports_its_mouse_route() {
        let (mut output, fake) = Output::fake();
        assert!(output.mouse_route());
        fake.set_kinds(0x02);
        let device = output.connect().unwrap();
        assert!(!device.mouse_route);
        assert!(!output.mouse_route());
        assert_eq!(device.firmware_label(), "V4.094");
    }

    #[test]
    fn a_percent_interpolation_is_confirmed_by_its_echo() {
        let (mut output, fake) = Output::fake();
        output.set_interpolation(percent(40)).unwrap();
        assert_eq!(last_frame(&fake), frame(Query::Interpolate(40)));
    }

    #[test]
    fn auto_interpolation_is_sent_as_255() {
        let (mut output, fake) = Output::fake();
        output.set_interpolation(Interpolation::Auto).unwrap();
        assert_eq!(last_frame(&fake), frame(Query::Interpolate(255)));
    }

    #[test]
    fn auto_interpolation_accepts_any_reply() {
        let (mut output, fake) = Output::fake();
        fake.set_interpolate_echo(7);
        output.set_interpolation(Interpolation::Auto).unwrap();
    }

    #[test]
    fn an_interpolation_without_a_reply_is_an_error_and_ends_the_link() {
        let (mut output, fake) = Output::fake();
        fake.mute_next(protocol::CMD_INTERPOLATE);
        assert!(output.set_interpolation(percent(40)).is_err());
        assert!(!output.is_connected());
    }

    #[test]
    fn an_interpolation_whose_echo_differs_is_an_error_and_keeps_the_link() {
        let (mut output, fake) = Output::fake();
        fake.set_interpolate_echo(7);
        assert!(output.set_interpolation(percent(40)).is_err());
        assert!(output.is_connected());
    }

    #[test]
    fn a_rejected_interpolation_is_an_error_and_keeps_the_link() {
        let (mut output, fake) = Output::fake();
        fake.reject_next(protocol::CMD_INTERPOLATE);
        assert!(output.set_interpolation(percent(40)).is_err());
        assert!(output.is_connected());
    }

    #[test]
    fn an_interpolation_without_a_link_is_an_error() {
        let mut output = Output::with_connector(Arc::new(fake::FakeMakcu::new()));
        assert!(output.set_interpolation(percent(40)).is_err());
    }
}
