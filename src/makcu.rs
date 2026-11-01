use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};

use self::{
    port::{
        Connector, ManualConnector,
        windows::{WindowsConnector, available_ports},
    },
    protocol::{Request, is_interpolate_echo, routes},
    session::{Session, SessionEvent, SessionInfo},
};
use crate::settings::{Interpolation, InterpolationPercent};

mod port;
mod protocol;
mod session;

pub use port::PortEntry;
#[cfg(test)]
pub(crate) use port::fake::FakeMakcu;
pub use protocol::format_firmware;

const RECONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(25);
const RECONNECT_RETRY_WINDOW: Duration = Duration::from_millis(500);
/// The wire value of `INTERPOLATE` for Auto.
const INTERPOLATE_AUTO: u8 = 255;

#[derive(Debug, Clone, Default)]
pub struct MakcuRoutes {
    pub mouse: bool,
}

/// The mouse movement that the engine sends for one frame, in whole counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Movement {
    pub x: i16,
    pub y: i16,
}

#[derive(Debug, Clone)]
pub struct MakcuMetadata {
    pub port: String,
    pub firmware: u32,
    pub baud: u32,
    pub routes: MakcuRoutes,
    /// The stored Move interpolation could not be sent on this connect. The connect still succeeded.
    pub interpolation_error: Option<String>,
}

#[derive(Debug)]
pub enum MakcuConnectionEvent {
    Disconnected,
    Connecting,
    Connected(Result<MakcuMetadata, String>),
}

impl std::fmt::Debug for MakcuClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MakcuClient").finish_non_exhaustive()
    }
}

pub struct MakcuClient {
    session: Option<Session>,
    routes: MakcuRoutes,
    port: Option<String>,
    /// The port that the user chose. `None` is auto-detect.
    selected_port: Option<String>,
    /// The wire value of the Move interpolation. Each connect sends it, so it is kept across reconnects.
    interpolation: Arc<AtomicU8>,
}

impl Default for MakcuClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MakcuClient {
    #[cfg(test)]
    pub(crate) fn fake() -> (Self, FakeMakcu) {
        let fake = FakeMakcu::new();
        (Self::fake_with(fake.clone()), fake)
    }

    /// A client connected to `fake`, which the test can change before or after the connect.
    #[cfg(test)]
    pub(crate) fn fake_with(fake: FakeMakcu) -> Self {
        let mut client = Self::new();
        let (session, info) =
            Session::connect(Arc::new(fake), None, Arc::clone(&client.interpolation))
                .expect("the fake MAKCU connects");
        client.session = Some(session);
        client.adopt(info);
        client
    }

    pub fn new() -> Self {
        Self {
            session: None,
            routes: MakcuRoutes::default(),
            port: None,
            selected_port: None,
            interpolation: Arc::new(AtomicU8::new(INTERPOLATE_AUTO)),
        }
    }

    /// Chooses the port of the next connect. `None` is auto-detect. A chosen port is the only one
    /// that is tried, also by the reconnects of the session.
    pub fn set_port(&mut self, port: Option<String>) {
        self.selected_port = port;
    }

    /// Every serial port of the system, for the port selector.
    pub fn available_ports() -> Result<Vec<PortEntry>> {
        Ok(available_ports()?)
    }

    fn connector(&self) -> Arc<dyn Connector> {
        match &self.selected_port {
            Some(port) => Arc::new(ManualConnector::new(
                Arc::new(WindowsConnector),
                port.clone(),
            )),
            None => Arc::new(WindowsConnector),
        }
    }

    pub fn connect_initial(&mut self) -> Result<MakcuMetadata> {
        tracing::info!(selected_port = ?self.selected_port, "connecting to MAKCU device");
        self.connect(self.connector(), None)
    }

    pub fn refresh_routes(&mut self) -> Result<MakcuMetadata> {
        tracing::info!(preferred_port = ?self.port, "refreshing MAKCU routes");
        let preferred = self.port.clone();
        self.close();
        self.connect_within_window(&self.connector(), preferred.as_deref())
    }

    pub fn rescan(&mut self) -> Result<MakcuMetadata> {
        tracing::info!(selected_port = ?self.selected_port, "rescanning MAKCU devices");
        self.connect(self.connector(), None)
    }

    pub fn poll_connection_event(&mut self) -> Option<MakcuConnectionEvent> {
        let event = self.session.as_ref()?.poll_event()?;
        Some(match event {
            SessionEvent::Disconnected => {
                tracing::warn!("MAKCU disconnected");
                self.routes = MakcuRoutes::default();
                MakcuConnectionEvent::Disconnected
            }
            SessionEvent::Connecting => {
                tracing::info!("MAKCU reconnecting");
                self.routes = MakcuRoutes::default();
                MakcuConnectionEvent::Connecting
            }
            SessionEvent::Connected(Ok(info)) => {
                tracing::info!("MAKCU connected; refreshing metadata");
                MakcuConnectionEvent::Connected(Ok(self.adopt(info)))
            }
            SessionEvent::Connected(Err(error)) => MakcuConnectionEvent::Connected(Err(error)),
        })
    }

    /// The Move interpolation that the client sends on each connect.
    pub fn interpolation(&self) -> Interpolation {
        match self.interpolation.load(Ordering::Acquire) {
            INTERPOLATE_AUTO => Interpolation::Auto,
            percent => Interpolation::Percent(InterpolationPercent::new(u16::from(percent))),
        }
    }

    pub fn mouse_available(&self) -> bool {
        self.routes.mouse
    }

    pub fn is_connected(&self) -> bool {
        self.session.as_ref().is_some_and(Session::is_connected)
    }

    /// Stores the Move interpolation and, when connected, sends it. A reply that does not confirm
    /// the value is an error. While disconnected the value is only stored, and the next connect sends it.
    pub fn set_interpolation(&mut self, interpolation: Interpolation) -> Result<()> {
        let value = interpolation.wire_value();
        self.interpolation.store(value, Ordering::Release);
        let Some(session) = self
            .session
            .as_ref()
            .filter(|session| session.is_connected())
        else {
            return Ok(());
        };
        let packet = session
            .request(Request::Interpolate(value))
            .context("MAKCU did not confirm the move interpolation")?;
        ensure!(
            is_interpolate_echo(value, &packet),
            "MAKCU answered the move interpolation with {:?}",
            packet.payload()
        );
        Ok(())
    }

    /// Writes one movement. It never waits for a reply.
    pub fn move_mouse(&mut self, movement: Movement) -> Result<()> {
        if !self.mouse_available() {
            bail!("MAKCU reports no mouse route");
        }
        self.write_move(movement)
    }

    /// Writes one move as one port write.
    fn write_move(&mut self, movement: Movement) -> Result<()> {
        let session = self.session.as_ref().context("MAKCU is not connected")?;
        session
            .send(&[Request::Move {
                x: movement.x,
                y: movement.y,
            }])
            .context("MAKCU mouse move failed")?;
        Ok(())
    }

    /// Ends the session and connects again. The port is closed before the new connect.
    fn connect(
        &mut self,
        connector: Arc<dyn Connector>,
        preferred: Option<&str>,
    ) -> Result<MakcuMetadata> {
        self.close();
        let (session, info) =
            Session::connect(connector, preferred, Arc::clone(&self.interpolation))?;
        self.session = Some(session);
        Ok(self.adopt(info))
    }

    /// Tries the preferred port again for the retry window. The session falls back to the other ports on each try.
    fn connect_within_window(
        &mut self,
        connector: &Arc<dyn Connector>,
        preferred: Option<&str>,
    ) -> Result<MakcuMetadata> {
        let started = Instant::now();
        loop {
            match Session::connect(
                Arc::clone(connector),
                preferred,
                Arc::clone(&self.interpolation),
            ) {
                Ok((session, info)) => {
                    self.session = Some(session);
                    return Ok(self.adopt(info));
                }
                Err(error) if started.elapsed() >= RECONNECT_RETRY_WINDOW => return Err(error),
                Err(_) => thread::sleep(RECONNECT_RETRY_INTERVAL),
            }
        }
    }

    /// Drops the session, which closes the port, and forgets the routes and the Raw ledger.
    fn close(&mut self) {
        self.session = None;
        self.routes = MakcuRoutes::default();
    }

    /// Stores the routes and the port for a connected session, and returns its metadata.
    fn adopt(&mut self, info: SessionInfo) -> MakcuMetadata {
        let routes = routes(info.kinds);
        tracing::info!(
            port = %info.port,
            firmware = info.firmware,
            baud = info.baud,
            mouse_route = routes.mouse,
            "MAKCU metadata refreshed"
        );
        self.routes = routes.clone();
        self.port = Some(info.port.clone());
        MakcuMetadata {
            port: info.port,
            firmware: info.firmware,
            baud: info.baud,
            routes,
            interpolation_error: info.interpolation_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::InterpolationPercent;

    /// The frames that the fake received after the connect.
    fn last_frame(fake: &FakeMakcu) -> Vec<u8> {
        fake.frames().last().unwrap().clone()
    }

    fn step(x: i16, y: i16) -> Movement {
        Movement { x, y }
    }

    #[test]
    fn the_client_reports_the_interpolation_that_it_sends_on_connect() {
        let mut client = MakcuClient::new();
        assert_eq!(client.interpolation(), Interpolation::Auto);
        let percent = Interpolation::Percent(InterpolationPercent::new(40));
        client.set_interpolation(percent).unwrap();
        assert_eq!(client.interpolation(), percent);
    }

    #[test]
    fn a_standard_move_writes_the_move_frame() {
        let (mut client, fake) = MakcuClient::fake();
        client.move_mouse(step(10, -5)).unwrap();
        assert_eq!(
            last_frame(&fake),
            [0xDE, 0xAD, 4, 0, 0x18, 10, 0, 0xFB, 0xFF]
        );
    }

    #[test]
    fn a_move_without_a_mouse_route_is_an_error_and_writes_nothing() {
        let (mut client, fake) = MakcuClient::fake();
        client.routes = MakcuRoutes::default();
        let frames = fake.frames().len();
        let error = client.move_mouse(step(1, 1)).unwrap_err();
        assert_eq!(error.to_string(), "MAKCU reports no mouse route");
        assert_eq!(fake.frames().len(), frames);
    }

    #[test]
    fn the_fake_client_is_connected_with_the_mouse_route() {
        let (client, _fake) = MakcuClient::fake();
        assert!(client.is_connected());
        assert!(client.mouse_available());
    }

    #[test]
    fn firmware_under_the_floor_makes_the_connect_fail_with_the_floor_message() {
        let fake = FakeMakcu::new();
        fake.set_firmware(4075);
        let mut client = MakcuClient::new();
        let error = client.connect(Arc::new(fake), None).unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "MAKCU firmware V4.075 is older than V4.076"
        );
        assert!(!client.is_connected());
    }

    #[test]
    fn a_percent_interpolation_is_confirmed_by_its_echo() {
        let (mut client, fake) = MakcuClient::fake();
        client
            .set_interpolation(Interpolation::Percent(InterpolationPercent::new(40)))
            .unwrap();
        assert_eq!(last_frame(&fake), [0xDE, 0xAD, 1, 0, 0x1F, 40]);
    }

    #[test]
    fn auto_interpolation_is_sent_as_255() {
        let (mut client, fake) = MakcuClient::fake();
        client.set_interpolation(Interpolation::Auto).unwrap();
        assert_eq!(last_frame(&fake), [0xDE, 0xAD, 1, 0, 0x1F, 0xFF]);
    }

    #[test]
    fn a_muted_interpolate_is_an_error() {
        let (mut client, fake) = MakcuClient::fake();
        fake.mute_next(0x1F);
        assert!(
            client
                .set_interpolation(Interpolation::Percent(InterpolationPercent::new(40)))
                .is_err()
        );
    }

    #[test]
    fn a_percent_interpolation_whose_echo_differs_is_an_error() {
        let (mut client, fake) = MakcuClient::fake();
        fake.set_interpolate_echo(7);
        assert!(
            client
                .set_interpolation(Interpolation::Percent(InterpolationPercent::new(40)))
                .is_err()
        );
    }

    #[test]
    fn auto_interpolation_accepts_any_reply() {
        let (mut client, fake) = MakcuClient::fake();
        fake.set_interpolate_echo(7);
        client.set_interpolation(Interpolation::Auto).unwrap();
    }

    /// Waits until the I/O thread reports that the link is down.
    fn wait_for_disconnect(client: &mut MakcuClient) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match client.poll_connection_event() {
                Some(MakcuConnectionEvent::Disconnected) => return,
                Some(_) => {}
                None => {
                    assert!(Instant::now() < deadline, "no disconnect event");
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    /// Waits until the reconnect is done. The connect has sent the stored values by then.
    fn wait_for_reconnect(client: &mut MakcuClient) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match client.poll_connection_event() {
                Some(MakcuConnectionEvent::Connected(Ok(_))) => return,
                Some(_) => {}
                None => {
                    assert!(Instant::now() < deadline, "no reconnect event");
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    #[test]
    fn a_stored_interpolation_is_sent_by_the_next_connect() {
        let (mut client, fake) = MakcuClient::fake();
        fake.disconnect();
        wait_for_disconnect(&mut client);
        // While disconnected the value is only stored, so nothing is sent yet.
        client
            .set_interpolation(Interpolation::Percent(InterpolationPercent::new(60)))
            .unwrap();
        assert_ne!(last_frame(&fake), [0xDE, 0xAD, 1, 0, 0x1F, 60]);

        fake.reconnect();
        wait_for_reconnect(&mut client);
        assert_eq!(last_frame(&fake), [0xDE, 0xAD, 1, 0, 0x1F, 60]);
    }
}
