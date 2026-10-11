//! The session with MAKCU: it connects, reads the replies on one I/O thread, and reconnects after a drop.
//!
//! The protocol is in `docs/makcu-protocol.md`. The port is in `port`.

use std::{
    fmt, io,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, bounded, unbounded};

use super::{
    port::{Connector, Port},
    protocol::{
        FIRMWARE_FLOOR, Packet, PacketDecoder, RawMetadata, Request, format_firmware,
        is_interpolate_echo, is_raw_reply, is_reject, parse_raw_metadata,
    },
};

const LOW_BAUD: u32 = 115_200;
const HIGH_BAUD: u32 = 4_000_000;
const REPLY_TIMEOUT: Duration = Duration::from_millis(100);
const FIRST_RETRY: Duration = Duration::from_millis(100);
const MAX_RETRY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionInfo {
    pub port: String,
    pub firmware: u32,
    pub kinds: u8,
    pub raw: Option<RawMetadata>,
    pub baud: u32,
    /// The stored Move interpolation could not be sent on this connect. The connect still succeeds.
    pub interpolation_error: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SessionEvent {
    Disconnected,
    Connecting,
    Connected(Result<SessionInfo, String>),
}

/// Why a request failed. The caller decides from this, so the variants are the cases it must tell apart.
#[derive(Debug)]
pub(super) enum SessionError {
    /// MAKCU sent the `FF` reject for this request. The connection is still up.
    Rejected,
    /// MAKCU did not answer in time. The connection is ended and reconnected.
    TimedOut,
    /// The link is down before the request or while it waited. Nothing was answered.
    NotConnected,
    /// The port failed while the request was written.
    Io(io::Error),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected => write!(f, "MAKCU rejected the request"),
            Self::TimedOut => write!(f, "MAKCU did not answer in time"),
            Self::NotConnected => write!(f, "MAKCU is not connected"),
            Self::Io(error) => write!(f, "MAKCU port failed: {error}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<io::Error> for SessionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// The request that waits for its reply. Only one is open at a time.
struct Slot {
    request: Request,
    reply: Sender<Result<Packet, SessionError>>,
}

/// The state that the caller and the I/O thread share.
struct Shared {
    /// The write side of the port. `None` while the link is down: the port is closed then,
    /// because the reopen needs it closed.
    writer: Mutex<Option<Box<dyn Port>>>,
    slot: Mutex<Option<Slot>>,
    connected: AtomicBool,
}

impl Shared {
    /// The only place where the link goes down. The caller and the I/O thread both call it, so
    /// after it returns no request can use the old link.
    fn mark_lost(&self) {
        self.connected.store(false, Ordering::Release);
        *lock(&self.writer) = None;
        *lock(&self.slot) = None;
    }
}

pub(super) struct Session {
    shared: Arc<Shared>,
    /// Serializes the requests. A request that needs a reply takes this lock.
    requests: Mutex<()>,
    /// A timeout on a connection sends a fault here. Dropping the sender stops the I/O thread.
    faults: Sender<()>,
    events: Receiver<SessionEvent>,
    raw_replies: Receiver<Packet>,
    io: Option<JoinHandle<()>>,
}

impl Session {
    /// Connects to the first port that answers as MAKCU. The preferred port is tried first.
    /// `interpolation` is the wire value of the Move interpolation. Each connect, including a reconnect, sends it.
    pub(super) fn connect(
        connector: Arc<dyn Connector>,
        preferred: Option<&str>,
        interpolation: Arc<AtomicU8>,
    ) -> Result<(Self, SessionInfo)> {
        let (port, info) = connect_any(
            connector.as_ref(),
            preferred,
            interpolation.load(Ordering::Acquire),
        )?;
        let reader = port.try_clone()?;
        let session = Self::start(connector, port, reader, info.port.clone(), interpolation)?;
        Ok((session, info))
    }

    fn start(
        connector: Arc<dyn Connector>,
        writer: Box<dyn Port>,
        reader: Box<dyn Port>,
        preferred: String,
        interpolation: Arc<AtomicU8>,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            writer: Mutex::new(Some(writer)),
            slot: Mutex::new(None),
            connected: AtomicBool::new(true),
        });
        let (faults, fault_receiver) = unbounded();
        let (event_sender, events) = unbounded();
        let (raw_sender, raw_replies) = unbounded();
        let io = IoThread {
            connector,
            shared: Arc::clone(&shared),
            faults: fault_receiver,
            events: event_sender,
            raw: raw_sender,
            preferred,
            interpolation,
        };
        let io = thread::Builder::new()
            .name("makcu-io".to_owned())
            .spawn(move || io.run(reader))?;
        Ok(Self {
            shared,
            requests: Mutex::new(()),
            faults,
            events,
            raw_replies,
            io: Some(io),
        })
    }

    /// Sends a request and waits for its reply. A timeout ends the connection at once, so the
    /// next request fails with `NotConnected` until the reconnect is done.
    pub(super) fn request(&self, request: &Request) -> Result<Packet, SessionError> {
        let _turn = lock(&self.requests);
        if !self.is_connected() {
            return Err(SessionError::NotConnected);
        }
        let (reply, answer) = bounded(1);
        *lock(&self.shared.slot) = Some(Slot {
            request: *request,
            reply,
        });
        if let Err(error) = self.write(request) {
            *lock(&self.shared.slot) = None;
            return Err(error);
        }
        match answer.recv_timeout(REPLY_TIMEOUT) {
            Ok(reply) => reply,
            Err(RecvTimeoutError::Disconnected) => Err(SessionError::NotConnected),
            Err(RecvTimeoutError::Timeout) => {
                // A late reply has no tag. It would look like the answer to the next request of
                // the same command, so the link goes down here, before any other request can run.
                self.shared.mark_lost();
                let _ = self.faults.send(());
                Err(SessionError::TimedOut)
            }
        }
    }

    /// Writes a request that has no reply to wait for.
    pub(super) fn send(&self, request: &Request) -> Result<(), SessionError> {
        if !self.is_connected() {
            return Err(SessionError::NotConnected);
        }
        self.write(request)
    }

    pub(super) fn raw_replies(&self) -> &Receiver<Packet> {
        &self.raw_replies
    }

    pub(super) fn poll_event(&self) -> Option<SessionEvent> {
        self.events.try_recv().ok()
    }

    pub(super) fn is_connected(&self) -> bool {
        self.shared.connected.load(Ordering::Acquire)
    }

    fn write(&self, request: &Request) -> Result<(), SessionError> {
        let mut writer = lock(&self.shared.writer);
        let Some(port) = writer.as_mut() else {
            return Err(SessionError::NotConnected);
        };
        port.write_all(&encoded(request))?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Replacing the sender drops the old one, so the I/O thread sees the end and returns.
        self.faults = unbounded().0;
        if let Some(io) = self.io.take() {
            let _ = io.join();
        }
    }
}

#[derive(PartialEq, Eq)]
enum Ended {
    /// The link was lost. The I/O thread reconnects.
    Lost,
    /// The session was dropped. The I/O thread returns.
    Stopped,
}

struct IoThread {
    connector: Arc<dyn Connector>,
    shared: Arc<Shared>,
    faults: Receiver<()>,
    events: Sender<SessionEvent>,
    raw: Sender<Packet>,
    /// The port that was used last. It is tried first.
    preferred: String,
    /// The Move interpolation to send on each connect. The client changes it.
    interpolation: Arc<AtomicU8>,
}

impl IoThread {
    fn run(mut self, reader: Box<dyn Port>) {
        let mut next = Some(reader);
        while let Some(reader) = next.take() {
            if self.serve(reader) == Ended::Stopped {
                return;
            }
            self.lost();
            next = self.reconnect();
        }
    }

    /// Reads the replies until the link is lost or the session is dropped. The reader is closed on return.
    fn serve(&mut self, mut reader: Box<dyn Port>) -> Ended {
        let mut decoder = PacketDecoder::default();
        let mut buf = [0u8; 256];
        loop {
            match self.faults.try_recv() {
                Ok(()) => return Ended::Lost,
                Err(TryRecvError::Disconnected) => return Ended::Stopped,
                Err(TryRecvError::Empty) => {}
            }
            match reader.read(&mut buf) {
                Ok(count) => {
                    for packet in decoder.feed(&buf[..count]) {
                        self.route(packet);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => return Ended::Lost,
            }
        }
    }

    /// A reply goes to the waiting request when its command matches. Otherwise a Raw reply goes to
    /// the ledger, and any other reply is dropped.
    fn route(&self, packet: Packet) {
        let waiting = lock(&self.shared.slot).take_if(|slot| slot.request.command() == packet.cmd);
        if let Some(slot) = waiting {
            let answer = if is_reject(&slot.request, &packet) {
                Err(SessionError::Rejected)
            } else {
                Ok(packet)
            };
            let _ = slot.reply.send(answer);
        } else if is_raw_reply(packet.cmd) {
            let _ = self.raw.send(packet);
        }
    }

    fn lost(&self) {
        self.shared.mark_lost();
        self.emit(SessionEvent::Disconnected);
    }

    /// Tries the connect sequence until it works. Returns the new reader, or `None` when the session is dropped.
    fn reconnect(&mut self) -> Option<Box<dyn Port>> {
        let mut attempt = 0;
        loop {
            self.emit(SessionEvent::Connecting);
            if let Ok(reader) = self.try_connect() {
                return Some(reader);
            }
            if self.pause(backoff(attempt)) {
                return None;
            }
            attempt += 1;
        }
    }

    fn try_connect(&mut self) -> Result<Box<dyn Port>> {
        let interpolation = self.interpolation.load(Ordering::Acquire);
        let (port, info) = connect_any(
            self.connector.as_ref(),
            Some(&self.preferred),
            interpolation,
        )?;
        let reader = port.try_clone()?;
        self.preferred = info.port.clone();
        *lock(&self.shared.writer) = Some(port);
        self.shared.connected.store(true, Ordering::Release);
        // A fault that came during the reconnect is about the old connection.
        while self.faults.try_recv().is_ok() {}
        self.emit(SessionEvent::Connected(Ok(info)));
        Ok(reader)
    }

    /// Waits for `delay`. Returns `true` when the session was dropped.
    fn pause(&self, delay: Duration) -> bool {
        let deadline = Instant::now() + delay;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.faults.recv_timeout(left) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout) => return false,
                Err(RecvTimeoutError::Disconnected) => return true,
            }
        }
    }

    fn emit(&self, event: SessionEvent) {
        let _ = self.events.send(event);
    }
}

/// The wait before try `attempt` of a reconnect, counted from 0: 100 ms, doubled each time, at most 5 s.
fn backoff(attempt: u32) -> Duration {
    FIRST_RETRY
        .saturating_mul(2u32.saturating_pow(attempt))
        .min(MAX_RETRY)
}

/// Connects to the first port that answers as MAKCU. The error is the one of the last port.
fn connect_any(
    connector: &dyn Connector,
    preferred: Option<&str>,
    interpolation: u8,
) -> Result<(Box<dyn Port>, SessionInfo)> {
    let mut ports = connector.ports()?;
    if let Some(index) = preferred.and_then(|name| ports.iter().position(|port| port == name)) {
        let port = ports.remove(index);
        ports.insert(0, port);
    }
    let mut last_error = anyhow!("no MAKCU port found");
    for port in &ports {
        match connect_port(connector, port, interpolation) {
            Ok(connected) => return Ok(connected),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

/// The connect sequence on one port: probe both rates, check the firmware floor, switch to 4,000,000
/// baud when the port is at 115,200, then read the routes and the Raw metadata, and send the Move interpolation.
fn connect_port(
    connector: &dyn Connector,
    name: &str,
    interpolation: u8,
) -> Result<(Box<dyn Port>, SessionInfo)> {
    // The rate that MAKCU keeps is not known after the app closes, so both rates are probed.
    let (mut port, mut baud) = match probe(connector, name, LOW_BAUD) {
        Ok(port) => (port, LOW_BAUD),
        Err(_) => (probe(connector, name, HIGH_BAUD)?, HIGH_BAUD),
    };
    let firmware = firmware_of(port.as_mut())?;
    if firmware < FIRMWARE_FLOOR {
        bail!(
            "MAKCU firmware {} is older than {}",
            format_firmware(firmware),
            format_firmware(FIRMWARE_FLOOR)
        );
    }
    if baud == LOW_BAUD {
        // A `SetBaud` that the device accepts has no reply. A refused one leaves the device at 115,200.
        port.write_all(&encoded(&Request::SetBaud(HIGH_BAUD)))?;
        // Close the port before the reopen: Windows does not open a COM port twice.
        drop(port);
        (port, baud) = match probe(connector, name, HIGH_BAUD) {
            Ok(port) => (port, HIGH_BAUD),
            Err(_) => (probe(connector, name, LOW_BAUD)?, LOW_BAUD),
        };
    }
    let kinds = kinds_of(port.as_mut())?;
    let raw = exchange(port.as_mut(), &Request::RawMetadata)
        .ok()
        .and_then(|packet| parse_raw_metadata(&packet.payload));
    // The device can lose the value when it restarts, so it is sent on every connect. A failure
    // does not fail the connect: the caller reports it.
    let interpolation_error = send_interpolation(port.as_mut(), interpolation)
        .err()
        .map(|error| format!("{error:#}"));
    let info = SessionInfo {
        port: name.to_owned(),
        firmware,
        kinds,
        raw,
        baud,
        interpolation_error,
    };
    Ok((port, info))
}

/// Sends `INTERPOLATE` and checks the echo.
fn send_interpolation(port: &mut dyn Port, value: u8) -> Result<()> {
    let packet = exchange(port, &Request::Interpolate(value))?;
    if !is_interpolate_echo(value, &packet) {
        bail!(
            "MAKCU answered the move interpolation {value} with {:?}",
            packet.payload
        );
    }
    Ok(())
}

/// Opens the port at `baud` and asks for the routes. The port is kept only when the device answers.
fn probe(connector: &dyn Connector, name: &str, baud: u32) -> Result<Box<dyn Port>> {
    let mut port = connector.open(name, baud)?;
    exchange(port.as_mut(), &Request::Device)?;
    Ok(port)
}

fn firmware_of(port: &mut dyn Port) -> Result<u32> {
    let packet = exchange(port, &Request::FirmwareVersion)?;
    let [a, b, c, d] = *packet.payload.as_slice() else {
        bail!("MAKCU sent a malformed firmware version");
    };
    Ok(u32::from_le_bytes([a, b, c, d]))
}

fn kinds_of(port: &mut dyn Port) -> Result<u8> {
    let packet = exchange(port, &Request::Device)?;
    let [kinds] = *packet.payload.as_slice() else {
        bail!("MAKCU sent a malformed device reply");
    };
    Ok(kinds)
}

/// Writes a request and reads until its reply. Used only before the I/O thread starts, or on the
/// I/O thread's own port while it reconnects.
fn exchange(port: &mut dyn Port, request: &Request) -> Result<Packet> {
    port.write_all(&encoded(request))?;
    let mut decoder = PacketDecoder::default();
    let mut buf = [0u8; 256];
    let deadline = Instant::now() + REPLY_TIMEOUT;
    while Instant::now() < deadline {
        let count = match port.read(&mut buf) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(error) => return Err(error.into()),
        };
        for packet in decoder.feed(&buf[..count]) {
            if packet.cmd == request.command() {
                if is_reject(request, &packet) {
                    bail!("MAKCU rejected {request:?}");
                }
                return Ok(packet);
            }
        }
    }
    bail!("MAKCU did not answer {request:?} in time")
}

fn encoded(request: &Request) -> Vec<u8> {
    let mut bytes = Vec::new();
    request.encode(&mut bytes);
    bytes
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::makcu::{port::fake::FakeMakcu, protocol::routes};

    const SET_BAUD_4M: [u8; 9] = [0xDE, 0xAD, 5, 0, 0xA5, 0x00, 0x09, 0x3D, 0x00];

    fn connect(fake: &FakeMakcu) -> Result<(Session, SessionInfo)> {
        Session::connect(Arc::new(fake.clone()), None, Arc::new(AtomicU8::new(255)))
    }

    fn next_event(session: &Session) -> SessionEvent {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(event) = session.poll_event() {
                return event;
            }
            assert!(Instant::now() < deadline, "no session event within 2 s");
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Waits for the reconnect to finish, skipping the `Connecting` events of the tries before it.
    fn wait_for_connected(session: &Session) -> SessionInfo {
        loop {
            match next_event(session) {
                SessionEvent::Connecting => {}
                SessionEvent::Connected(Ok(info)) => return info,
                other => panic!("unexpected event {other:?}"),
            }
        }
    }

    #[test]
    fn connect_switches_the_device_to_4_000_000_baud() {
        let fake = FakeMakcu::new();
        let (session, info) = connect(&fake).unwrap();
        assert_eq!(info.baud, HIGH_BAUD);
        assert!(info.raw.is_some());
        assert!(fake.frames().contains(&SET_BAUD_4M.to_vec()));
        assert!(session.is_connected());
    }

    #[test]
    fn connect_keeps_115_200_baud_when_the_device_refuses_the_switch() {
        let fake = FakeMakcu::new();
        fake.set_accepted_bauds(&[LOW_BAUD]);
        let (session, info) = connect(&fake).unwrap();
        assert_eq!(info.baud, LOW_BAUD);
        assert!(fake.frames().contains(&SET_BAUD_4M.to_vec()));
        assert!(session.is_connected());
    }

    #[test]
    fn connect_finds_a_device_already_at_4_000_000_baud_and_skips_the_switch() {
        let fake = FakeMakcu::new();
        fake.set_baud(HIGH_BAUD);
        fake.set_accepted_bauds(&[HIGH_BAUD]);
        let (session, info) = connect(&fake).unwrap();
        assert_eq!(info.baud, HIGH_BAUD);
        assert!(!fake.frames().iter().any(|frame| frame[4] == 0xA5));
        // The probe at 115,200 timed out during the connect, and no event says so.
        assert_eq!(session.poll_event(), None);
    }

    #[test]
    fn firmware_4084_is_refused_with_the_floor_message() {
        let fake = FakeMakcu::new();
        fake.set_firmware(4084);
        let error = connect(&fake).err().unwrap();
        assert_eq!(
            error.to_string(),
            "MAKCU firmware V4.084 is older than V4.085"
        );
    }

    #[test]
    fn firmware_at_the_floor_and_above_is_accepted() {
        for firmware in [4085, 4094] {
            let fake = FakeMakcu::new();
            fake.set_firmware(firmware);
            let (_session, info) = connect(&fake).unwrap();
            assert_eq!(info.firmware, firmware);
        }
    }

    #[test]
    fn routes_follow_the_mouse_bit() {
        let fake = FakeMakcu::new();
        fake.set_kinds(0x03);
        let (session, info) = connect(&fake).unwrap();
        assert!(routes(info.kinds).mouse);
        // The first session ends first: its I/O thread would read the replies of the next connect.
        drop(session);

        fake.set_kinds(0x02);
        let (_session, info) = connect(&fake).unwrap();
        assert!(!routes(info.kinds).mouse);
    }

    #[test]
    fn a_reject_frame_is_an_error_for_the_open_request() {
        let fake = FakeMakcu::new();
        let (session, _) = connect(&fake).unwrap();
        fake.reject_next(0x1F);
        let error = session.request(&Request::Interpolate(40)).unwrap_err();
        assert!(matches!(error, SessionError::Rejected));
        assert_eq!(error.to_string(), "MAKCU rejected the request");
        assert!(
            session.is_connected(),
            "a reject does not end the connection"
        );
    }

    #[test]
    fn the_auto_echo_of_interpolate_255_is_an_answer_not_a_reject() {
        let fake = FakeMakcu::new();
        let (session, _) = connect(&fake).unwrap();
        let reply = session.request(&Request::Interpolate(255)).unwrap();
        assert_eq!(reply.payload, [0xFF]);
    }

    #[test]
    fn a_late_reply_after_a_timeout_does_not_answer_the_next_request() {
        let fake = FakeMakcu::new();
        let (session, _) = connect(&fake).unwrap();
        fake.mute_next(0x1F);
        let error = session.request(&Request::Interpolate(40)).unwrap_err();
        assert!(matches!(error, SessionError::TimedOut));
        // The late reply arrives before the reconnect empties the input buffer.
        fake.inject(&[0xDE, 0xAD, 1, 0, 0x1F, 17]);

        assert_eq!(next_event(&session), SessionEvent::Disconnected);
        assert_eq!(next_event(&session), SessionEvent::Connecting);
        assert!(matches!(
            next_event(&session),
            SessionEvent::Connected(Ok(_))
        ));
        let reply = session.request(&Request::Interpolate(40)).unwrap();
        assert_eq!(reply.payload, [40]);
    }

    #[test]
    fn a_request_right_after_a_timeout_does_not_use_the_old_connection() {
        let fake = FakeMakcu::new();
        let (session, _) = connect(&fake).unwrap();
        fake.mute_next(0x1F);
        assert!(session.request(&Request::Interpolate(40)).is_err());
        fake.inject(&[0xDE, 0xAD, 1, 0, 0x1F, 17]);
        // No event is read first: the next request starts at once, as the caller can do.
        // The link went down in the timed-out call, so the request fails before it is written.
        let error = session.request(&Request::Interpolate(40)).unwrap_err();
        assert!(matches!(error, SessionError::NotConnected));
        assert!(fake.frames().last().is_some_and(|frame| frame[4] == 0x1F));
    }

    #[test]
    fn a_disconnect_emits_the_events_and_the_session_reconnects_after_the_reconnect() {
        let fake = FakeMakcu::new();
        let (session, _) = connect(&fake).unwrap();
        fake.disconnect();
        assert_eq!(next_event(&session), SessionEvent::Disconnected);
        assert_eq!(next_event(&session), SessionEvent::Connecting);
        assert!(!session.is_connected());
        fake.reconnect();
        let info = wait_for_connected(&session);
        assert_eq!(info.baud, HIGH_BAUD);
        assert!(session.is_connected());
    }

    #[test]
    fn the_reconnect_back_off_starts_at_100_ms_doubles_and_stops_at_5_s() {
        let delays: Vec<u64> = (0..8)
            .map(|attempt| backoff(attempt).as_millis() as u64)
            .collect();
        assert_eq!(delays, [100, 200, 400, 800, 1600, 3200, 5000, 5000]);
    }
}
