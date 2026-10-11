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
    port::{Connector, windows::WindowsConnector},
    protocol::{RawLedger, Request, is_interpolate_echo, parse_raw_reply, routes},
    session::{Session, SessionEvent, SessionInfo},
};
use crate::settings::{Interpolation, MoveMethod};

mod port;
mod protocol;
mod session;

#[cfg(test)]
pub(crate) use port::fake::FakeMakcu;
pub(crate) use protocol::{format_firmware, reject_reason_text};

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

/// Counters of the Raw moves. See `docs/makcu-protocol.md`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    /// The stored Move interpolation could not be sent on this connect. The connect still succeeded.
    pub interpolation_error: Option<String>,
}

#[derive(Debug)]
pub enum MakcuConnectionEvent {
    Disconnected,
    Connecting,
    Connected(Result<MakcuMetadata, String>),
}

pub struct MakcuClient {
    session: Option<Session>,
    routes: MakcuRoutes,
    port: Option<String>,
    move_method: MoveMethod,
    /// The wire value of the Move interpolation. Each connect sends it, so it is kept across reconnects.
    interpolation: Arc<AtomicU8>,
    /// The Raw move ledger of the current connection. `None` when the firmware gave no Raw metadata.
    raw: Option<RawLedger>,
    /// Set after the one warning that Raw move fell back to the Standard move on this connection.
    raw_fallback_warned: bool,
    /// The requests of one move. It is kept between moves, so a move does not allocate.
    batch: Vec<Request>,
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
            move_method: MoveMethod::default(),
            interpolation: Arc::new(AtomicU8::new(INTERPOLATE_AUTO)),
            raw: None,
            raw_fallback_warned: false,
            batch: Vec::new(),
        }
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

    /// Stores the method. It takes effect on the next `move_mouse`.
    pub fn set_move_method(&mut self, method: MoveMethod) {
        self.move_method = method;
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
            .request(&Request::Interpolate(value))
            .context("MAKCU did not confirm the move interpolation")?;
        ensure!(
            is_interpolate_echo(value, &packet),
            "MAKCU answered the move interpolation with {:?}",
            packet.payload()
        );
        Ok(())
    }

    /// Writes one movement with the method in use. It never waits for a reply.
    pub fn move_mouse(&mut self, movement: Movement) -> Result<()> {
        if !self.mouse_available() {
            bail!("MAKCU reports no mouse route");
        }
        let started = Instant::now();
        self.write_move(movement)?;
        if crate::logging::makcu_command_trace_enabled() {
            tracing::trace!(
                x = movement.x,
                y = movement.y,
                elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
                "MAKCU mouse command completed"
            );
        }
        Ok(())
    }

    /// The Raw move counters of this connection. The counters start again at each connect.
    pub fn raw_stats(&self) -> RawStats {
        self.raw.as_ref().map(RawLedger::stats).unwrap_or_default()
    }

    /// Writes one move as one port write. A Raw move first takes the replies that arrived, then writes
    /// its request and the peeks and consumes that are due. When Raw move cannot run, the Standard move is written.
    fn write_move(&mut self, movement: Movement) -> Result<()> {
        let session = self.session.as_ref().context("MAKCU is not connected")?;
        if self.move_method == MoveMethod::Raw {
            match self.raw.as_mut() {
                Some(ledger) => {
                    while let Ok(packet) = session.raw_replies().try_recv() {
                        if let Some(reply) = parse_raw_reply(&packet) {
                            ledger.on_reply(reply);
                        }
                    }
                    if let Some(submit) = ledger.next_submit(movement.x, movement.y) {
                        self.batch.clear();
                        self.batch.push(submit);
                        ledger.due_requests(&mut self.batch);
                        session
                            .send(&self.batch)
                            .context("MAKCU raw mouse move failed")?;
                        return Ok(());
                    }
                    warn_raw_fallback(&mut self.raw_fallback_warned, "the Raw move ids ran out");
                }
                None => {
                    warn_raw_fallback(&mut self.raw_fallback_warned, "MAKCU gave no Raw metadata")
                }
            }
        }
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
        connector: Arc<dyn Connector>,
        preferred: Option<&str>,
    ) -> Result<MakcuMetadata> {
        let started = Instant::now();
        loop {
            match Session::connect(
                Arc::clone(&connector),
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
        self.raw = None;
    }

    /// Stores the routes, the port, and a new Raw ledger for a connected session, and returns its metadata.
    /// The counters of the Raw moves are reset with the ledger, so they cover one connection.
    fn adopt(&mut self, info: SessionInfo) -> MakcuMetadata {
        let routes = routes(info.kinds);
        tracing::info!(
            port = %info.port,
            firmware = info.firmware,
            baud = info.baud,
            mouse_route = routes.mouse,
            "MAKCU metadata refreshed"
        );
        // A reply that arrived before this connect belongs to the old ledger, so it is dropped.
        if let Some(session) = self.session.as_ref() {
            while session.raw_replies().try_recv().is_ok() {}
        }
        self.raw = info
            .raw
            .map(|raw| RawLedger::new(raw.last_accepted_id.saturating_add(1)));
        self.raw_fallback_warned = false;
        self.routes = routes.clone();
        self.port = Some(info.port.clone());
        MakcuMetadata {
            port: info.port,
            firmware: info.firmware,
            routes,
            interpolation_error: info.interpolation_error,
        }
    }
}

/// Logs the fallback from Raw move to the Standard move once per connection.
fn warn_raw_fallback(warned: &mut bool, reason: &str) {
    if !*warned {
        *warned = true;
        tracing::warn!(reason, "Raw move falls back to the Standard move");
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

    /// The frames of one command.
    fn frames_of(fake: &FakeMakcu, cmd: u8) -> Vec<Vec<u8>> {
        fake.frames()
            .into_iter()
            .filter(|frame| frame[4] == cmd)
            .collect()
    }

    fn step(x: i16, y: i16) -> Movement {
        Movement { x, y }
    }

    /// Moves until `done` holds, so that the replies the I/O thread reads are counted.
    fn settle(client: &mut MakcuClient, done: impl Fn(&MakcuClient) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !done(client) {
            assert!(Instant::now() < deadline, "the Raw replies did not arrive");
            client.move_mouse(step(0, 0)).unwrap();
            thread::sleep(Duration::from_millis(1));
        }
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
        fake.set_firmware(4084);
        let mut client = MakcuClient::new();
        let error = client.connect(Arc::new(fake), None).unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "MAKCU firmware V4.084 is older than V4.085"
        );
        assert!(!client.is_connected());
    }

    #[test]
    fn a_raw_move_writes_raw_frames_from_the_id_after_the_last_accepted_one() {
        let fake = FakeMakcu::new();
        fake.set_raw_last_accepted(4);
        let mut client = MakcuClient::fake_with(fake.clone());
        client.set_move_method(MoveMethod::Raw);
        client.move_mouse(step(5, -5)).unwrap();
        client.move_mouse(step(1, 1)).unwrap();

        assert!(frames_of(&fake, 0x18).is_empty());
        let raw = frames_of(&fake, 0x69);
        assert_eq!(raw.len(), 2);
        assert_eq!(
            raw[0],
            [
                0xDE, 0xAD, 0x0A, 0, 0x69, 5, 0, 0, 0, 5, 0, 0xFB, 0xFF, 0x20, 0x03
            ]
        );
        assert_eq!(raw[1][5..9], [6, 0, 0, 0]);
    }

    #[test]
    fn a_rejected_raw_move_is_counted_and_its_id_is_not_resent() {
        let fake = FakeMakcu::new();
        for id in 1..=7 {
            fake.set_raw_status(id, 3, 17);
        }
        let mut client = MakcuClient::fake_with(fake.clone());
        client.set_move_method(MoveMethod::Raw);
        // The eighth submit peeks the seven older ids, which all report a reject.
        for _ in 0..8 {
            client.move_mouse(step(1, 0)).unwrap();
        }
        settle(&mut client, |client| client.raw_stats().rejected >= 7);

        let stats = client.raw_stats();
        assert_eq!(stats.rejected, 7);
        assert_eq!(stats.last_reason, Some(17));
        assert_eq!(stats.timed_out, 0);
        let id_one_submits = frames_of(&fake, 0x69)
            .iter()
            .filter(|frame| frame[5..9] == [1, 0, 0, 0])
            .count();
        assert_eq!(id_one_submits, 1);
    }

    #[test]
    fn a_raw_move_with_its_due_peeks_is_one_port_write() {
        let fake = FakeMakcu::new();
        let mut client = MakcuClient::fake_with(fake.clone());
        client.set_move_method(MoveMethod::Raw);
        for _ in 0..8 {
            client.move_mouse(step(1, 0)).unwrap();
        }
        // The eighth submit writes its request and the seven peeks of the older ids together.
        let writes = fake.writes();
        let eighth = writes.last().unwrap();
        assert_eq!(eighth[4], 0x69);
        assert_eq!(eighth.len(), 15 + 7 * 10);
        let peeks: Vec<u8> = fake
            .frames()
            .iter()
            .rev()
            .take(7)
            .map(|frame| frame[5])
            .collect();
        assert_eq!(peeks, [7, 6, 5, 4, 3, 2, 1]);
    }

    #[test]
    fn a_raw_move_that_the_device_refuses_at_submit_is_counted_at_once() {
        let fake = FakeMakcu::new();
        // State 255 is an immediate error: the id was not accepted, so nothing waits for it.
        fake.set_raw_submit(1, 255, 3);
        let mut client = MakcuClient::fake_with(fake.clone());
        client.set_move_method(MoveMethod::Raw);
        client.move_mouse(step(1, 0)).unwrap();
        settle(&mut client, |client| client.raw_stats().rejected >= 1);
        assert_eq!(client.raw_stats().last_reason, Some(3));
    }

    #[test]
    fn a_raw_move_peeks_every_older_id_on_every_eighth_submit() {
        let fake = FakeMakcu::new();
        let mut client = MakcuClient::fake_with(fake.clone());
        client.set_move_method(MoveMethod::Raw);
        for _ in 0..8 {
            client.move_mouse(step(1, 0)).unwrap();
        }
        // Id 0 is the metadata query that the connect sends, so it is not a peek.
        let peeks: Vec<u8> = frames_of(&fake, 0x6A)
            .iter()
            .filter(|frame| frame[5..9] != [0, 0, 0, 0])
            .map(|frame| frame[5])
            .collect();
        assert_eq!(peeks, [1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn a_raw_move_falls_back_to_the_standard_move_without_raw_metadata() {
        let fake = FakeMakcu::new();
        fake.set_raw_metadata(false);
        let mut client = MakcuClient::fake_with(fake.clone());
        client.set_move_method(MoveMethod::Raw);
        client.move_mouse(step(3, 4)).unwrap();
        assert_eq!(last_frame(&fake), [0xDE, 0xAD, 4, 0, 0x18, 3, 0, 4, 0]);
        assert!(frames_of(&fake, 0x69).is_empty());
    }

    #[test]
    fn a_raw_move_falls_back_to_the_standard_move_when_the_ids_run_out() {
        let fake = FakeMakcu::new();
        fake.set_raw_last_accepted(u32::MAX - 1);
        let mut client = MakcuClient::fake_with(fake.clone());
        client.set_move_method(MoveMethod::Raw);
        client.move_mouse(step(1, 1)).unwrap();
        assert_eq!(frames_of(&fake, 0x69)[0][5..9], [0xFF; 4]);
        client.move_mouse(step(1, 1)).unwrap();
        assert_eq!(last_frame(&fake)[4], 0x18);
    }

    #[test]
    fn a_change_of_the_move_method_takes_effect_on_the_next_move() {
        let (mut client, fake) = MakcuClient::fake();
        client.move_mouse(step(1, 1)).unwrap();
        assert_eq!(last_frame(&fake)[4], 0x18);
        client.set_move_method(MoveMethod::Raw);
        client.move_mouse(step(1, 1)).unwrap();
        assert_eq!(last_frame(&fake)[4], 0x69);
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
