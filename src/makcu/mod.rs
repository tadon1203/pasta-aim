use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use self::{
    port::{Connector, windows::WindowsConnector},
    protocol::{Request, routes},
    session::{Session, SessionEvent, SessionInfo},
};

mod port;
#[expect(
    dead_code,
    reason = "the Raw move ledger and the Raw metadata are kept for ticket 05"
)]
mod protocol;
#[expect(
    dead_code,
    reason = "the request path and the Raw metadata are kept for ticket 05"
)]
mod session;

#[cfg(test)]
pub(crate) use port::fake::FakeMakcu;
pub(crate) use protocol::format_firmware;

const RECONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(25);
const RECONNECT_RETRY_WINDOW: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Default)]
pub struct MakcuRoutes {
    pub mouse: bool,
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

#[derive(Default)]
pub struct MakcuClient {
    session: Option<Session>,
    routes: MakcuRoutes,
    port: Option<String>,
}

impl MakcuClient {
    #[cfg(test)]
    pub(crate) fn fake() -> (Self, FakeMakcu) {
        let fake = FakeMakcu::new();
        let (session, info) =
            Session::connect(Arc::new(fake.clone()), None).expect("the fake MAKCU connects");
        let mut client = Self::new();
        client.session = Some(session);
        client.adopt(info);
        (client, fake)
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn connect_initial(&mut self) -> Result<MakcuMetadata> {
        tracing::info!("connecting to MAKCU device");
        self.connect(Arc::new(WindowsConnector), None)
    }

    pub fn refresh_routes(&mut self) -> Result<MakcuMetadata> {
        tracing::info!(preferred_port = ?self.port, "refreshing MAKCU routes");
        let preferred = self.port.clone();
        self.close();
        self.connect_within_window(Arc::new(WindowsConnector), preferred.as_deref())
    }

    pub fn rescan(&mut self) -> Result<MakcuMetadata> {
        tracing::info!("rescanning MAKCU devices");
        self.connect(Arc::new(WindowsConnector), None)
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

    pub fn mouse_available(&self) -> bool {
        self.routes.mouse
    }

    pub fn is_connected(&self) -> bool {
        self.session.as_ref().is_some_and(Session::is_connected)
    }

    /// Writes a Standard move. It never waits for a reply.
    pub fn move_mouse(&mut self, x: i32, y: i32) -> Result<()> {
        if !self.mouse_available() {
            bail!("MAKCU reports no mouse route");
        }
        let x = x.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let y = y.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let started = Instant::now();
        self.session
            .as_ref()
            .context("MAKCU is not connected")?
            .send(&Request::Move { x, y })
            .context("MAKCU mouse move failed")?;
        if crate::logging::makcu_command_trace_enabled() {
            tracing::trace!(
                x,
                y,
                elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
                "MAKCU mouse command completed"
            );
        }
        Ok(())
    }

    /// The Raw move counters. Raw moves are not sent yet, so they are all zero.
    #[expect(dead_code, reason = "the Raw move counters are read by ticket 05")]
    pub fn raw_stats(&self) -> RawStats {
        RawStats::default()
    }

    /// Ends the session and connects again. The port is closed before the new connect.
    fn connect(
        &mut self,
        connector: Arc<dyn Connector>,
        preferred: Option<&str>,
    ) -> Result<MakcuMetadata> {
        self.close();
        let (session, info) = Session::connect(connector, preferred)?;
        self.session = Some(session);
        Ok(self.adopt(info))
    }

    /// Tries the preferred port again for the retry window. The session falls back to the other ports on each try.
    fn connect_within_window(
        &mut self,
        connector: Arc<dyn Connector>,
        preferred: Option<&str>,
    ) -> Result<MakcuMetadata> {
        let started = Instant::now();
        loop {
            match Session::connect(Arc::clone(&connector), preferred) {
                Ok((session, info)) => {
                    self.session = Some(session);
                    return Ok(self.adopt(info));
                }
                Err(error) if started.elapsed() >= RECONNECT_RETRY_WINDOW => return Err(error),
                Err(_) => thread::sleep(RECONNECT_RETRY_INTERVAL),
            }
        }
    }

    /// Drops the session, which closes the port, and forgets the routes.
    fn close(&mut self) {
        self.session = None;
        self.routes = MakcuRoutes::default();
    }

    /// Stores the routes and the port of a connected session and returns its metadata.
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
            routes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frames that the fake received after the connect.
    fn last_frame(fake: &FakeMakcu) -> Vec<u8> {
        fake.frames().last().unwrap().clone()
    }

    #[test]
    fn a_standard_move_writes_the_move_frame() {
        let (mut client, fake) = MakcuClient::fake();
        client.move_mouse(10, -5).unwrap();
        assert_eq!(
            last_frame(&fake),
            [0xDE, 0xAD, 4, 0, 0x18, 10, 0, 0xFB, 0xFF]
        );
    }

    #[test]
    fn x_and_y_outside_i16_are_clamped() {
        let (mut client, fake) = MakcuClient::fake();
        client.move_mouse(100_000, -100_000).unwrap();
        assert_eq!(
            last_frame(&fake),
            [0xDE, 0xAD, 4, 0, 0x18, 0xFF, 0x7F, 0x00, 0x80]
        );
    }

    #[test]
    fn a_move_without_a_mouse_route_is_an_error_and_writes_nothing() {
        let (mut client, fake) = MakcuClient::fake();
        client.routes = MakcuRoutes::default();
        let frames = fake.frames().len();
        let error = client.move_mouse(1, 1).unwrap_err();
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
        fake.set_firmware(4084);
        let mut client = MakcuClient::new();
        let error = client.connect(Arc::new(fake), None).unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "MAKCU firmware V4.084 is older than V4.085"
        );
        assert!(!client.is_connected());
    }
}
