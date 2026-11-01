//! The session with MAKCU: it connects and reads the replies on one I/O thread. A drop ends the
//! session for good; the caller connects a new one.
//!
//! The protocol is in `docs/makcu-protocol.md`. The port is in `port`.

use std::{
    fmt, io,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use std::sync::mpsc::{RecvTimeoutError, SyncSender, sync_channel};

use super::{
    port::{Connector, Port},
    protocol::{
        FIRMWARE_FLOOR, Packet, PacketDecoder, Request, format_firmware, is_interpolate_echo,
        is_reject,
    },
};

const LOW_BAUD: u32 = 115_200;
const HIGH_BAUD: u32 = 4_000_000;
const REPLY_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionInfo {
    pub port: String,
    pub firmware: u32,
    pub kinds: u8,
    pub baud: u32,
    /// The stored Move interpolation could not be sent on this connect. The connect still succeeds.
    pub interpolation_error: Option<String>,
}

/// Why a request failed. The caller decides from this, so the variants are the cases it must tell apart.
#[derive(Debug)]
pub(super) enum SessionError {
    /// MAKCU sent the `FF` reject for this request. The connection is still up.
    Rejected,
    /// MAKCU did not answer in time. The connection is ended.
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
    reply: SyncSender<Result<Packet, SessionError>>,
}

/// The write side of the port, with the buffer that a write encodes into. One lock covers both.
struct Writer {
    port: Box<dyn Port>,
    buffer: Vec<u8>,
}

/// The state that the caller and the I/O thread share.
struct Shared {
    /// The write side of the port. `None` once the link is down: the port is closed then.
    writer: Mutex<Option<Writer>>,
    slot: Mutex<Option<Slot>>,
    connected: AtomicBool,
}

impl Shared {
    /// The only place where the link goes down, for good: a lost session is replaced, not repaired.
    /// The caller and the I/O thread both call it, so after it returns no request can use the link.
    /// The I/O thread sees the flag and returns.
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
    io: Option<JoinHandle<()>>,
}

impl Session {
    /// Connects to the first port that answers as MAKCU. The preferred port is tried first.
    /// `interpolation` is the wire value of the Move interpolation, sent on the connect.
    pub(super) fn connect(
        connector: &dyn Connector,
        preferred: Option<&str>,
        interpolation: u8,
    ) -> Result<(Self, SessionInfo)> {
        let (port, info) = connect_any(connector, preferred, interpolation)?;
        let reader = port.try_clone()?;
        let shared = Arc::new(Shared {
            writer: Mutex::new(Some(Writer {
                port,
                buffer: Vec::new(),
            })),
            slot: Mutex::new(None),
            connected: AtomicBool::new(true),
        });
        let io = thread::Builder::new().name("makcu-io".to_owned()).spawn({
            let shared = Arc::clone(&shared);
            move || serve(&shared, reader)
        })?;
        let session = Self {
            shared,
            requests: Mutex::new(()),
            io: Some(io),
        };
        Ok((session, info))
    }

    /// Sends a request and waits for its reply. A timeout ends the connection at once, so the
    /// next request fails with `NotConnected`.
    pub(super) fn request(&self, request: Request) -> Result<Packet, SessionError> {
        let _turn = lock(&self.requests);
        if !self.is_connected() {
            return Err(SessionError::NotConnected);
        }
        let (reply, answer) = sync_channel(1);
        *lock(&self.shared.slot) = Some(Slot { request, reply });
        if let Err(error) = self.write(std::slice::from_ref(&request)) {
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
                Err(SessionError::TimedOut)
            }
        }
    }

    /// Writes the requests as one port write. None of them has a reply to wait for.
    pub(super) fn send(&self, requests: &[Request]) -> Result<(), SessionError> {
        if !self.is_connected() {
            return Err(SessionError::NotConnected);
        }
        self.write(requests)
    }

    pub(super) fn is_connected(&self) -> bool {
        self.shared.connected.load(Ordering::Acquire)
    }

    fn write(&self, requests: &[Request]) -> Result<(), SessionError> {
        let mut guard = lock(&self.shared.writer);
        let Some(writer) = guard.as_mut() else {
            return Err(SessionError::NotConnected);
        };
        write_requests(writer.port.as_mut(), &mut writer.buffer, requests)?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.mark_lost();
        if let Some(io) = self.io.take() {
            let _ = io.join();
        }
    }
}

/// The I/O thread: reads the replies until the link is down, which a read error or the session
/// can cause. The reader is closed on return.
fn serve(shared: &Shared, mut reader: Box<dyn Port>) {
    let mut decoder = PacketDecoder::default();
    let mut buf = [0u8; 256];
    while shared.connected.load(Ordering::Acquire) {
        match reader.read(&mut buf) {
            Ok(count) => decoder.feed(&buf[..count], |packet| route(shared, packet)),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(_) => shared.mark_lost(),
        }
    }
}

/// A reply goes to the waiting request when its command matches. Any other reply is dropped.
fn route(shared: &Shared, packet: Packet) {
    let waiting = lock(&shared.slot).take_if(|slot| slot.request.command() == packet.cmd);
    if let Some(slot) = waiting {
        let answer = if is_reject(slot.request, &packet) {
            Err(SessionError::Rejected)
        } else {
            Ok(packet)
        };
        let _ = slot.reply.send(answer);
    }
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
/// baud when the port is at 115,200, then read the routes, and send the Move interpolation.
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
        write_requests(
            port.as_mut(),
            &mut Vec::new(),
            &[Request::SetBaud(HIGH_BAUD)],
        )?;
        // Close the port before the reopen: Windows does not open a COM port twice.
        drop(port);
        (port, baud) = match probe(connector, name, HIGH_BAUD) {
            Ok(port) => (port, HIGH_BAUD),
            Err(_) => (probe(connector, name, LOW_BAUD)?, LOW_BAUD),
        };
    }
    let kinds = kinds_of(port.as_mut())?;
    // The device can lose the value when it restarts, so it is sent on every connect. A failure
    // does not fail the connect: the caller reports it.
    let interpolation_error = send_interpolation(port.as_mut(), interpolation)
        .err()
        .map(|error| format!("{error:#}"));
    let info = SessionInfo {
        port: name.to_owned(),
        firmware,
        kinds,
        baud,
        interpolation_error,
    };
    Ok((port, info))
}

/// Sends `INTERPOLATE` and checks the echo.
fn send_interpolation(port: &mut dyn Port, value: u8) -> Result<()> {
    let packet = exchange(port, Request::Interpolate(value))?;
    if !is_interpolate_echo(value, &packet) {
        bail!(
            "MAKCU answered the move interpolation {value} with {:?}",
            packet.payload()
        );
    }
    Ok(())
}

/// Opens the port at `baud` and asks for the routes. The port is kept only when the device answers.
fn probe(connector: &dyn Connector, name: &str, baud: u32) -> Result<Box<dyn Port>> {
    let mut port = connector.open(name, baud)?;
    exchange(port.as_mut(), Request::Device)?;
    Ok(port)
}

fn firmware_of(port: &mut dyn Port) -> Result<u32> {
    let packet = exchange(port, Request::FirmwareVersion)?;
    let [a, b, c, d] = *packet.payload() else {
        bail!("MAKCU sent a malformed firmware version");
    };
    Ok(u32::from_le_bytes([a, b, c, d]))
}

fn kinds_of(port: &mut dyn Port) -> Result<u8> {
    let packet = exchange(port, Request::Device)?;
    let [kinds] = *packet.payload() else {
        bail!("MAKCU sent a malformed device reply");
    };
    Ok(kinds)
}

/// Writes a request and reads until its reply. Used only before the I/O thread starts.
fn exchange(port: &mut dyn Port, request: Request) -> Result<Packet> {
    write_requests(port, &mut Vec::new(), std::slice::from_ref(&request))?;
    let mut decoder = PacketDecoder::default();
    let mut buf = [0u8; 256];
    let mut answer = None;
    let deadline = Instant::now() + REPLY_TIMEOUT;
    while answer.is_none() && Instant::now() < deadline {
        let count = match port.read(&mut buf) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(error) => return Err(error.into()),
        };
        decoder.feed(&buf[..count], |packet| {
            if answer.is_none() && packet.cmd == request.command() {
                answer = Some(packet);
            }
        });
    }
    let Some(packet) = answer else {
        bail!("MAKCU did not answer {request:?} in time");
    };
    if is_reject(request, &packet) {
        bail!("MAKCU rejected {request:?}");
    }
    Ok(packet)
}

/// Encodes the requests into `buffer` and writes them with one port write. `buffer` is kept for the next write.
fn write_requests(
    port: &mut dyn Port,
    buffer: &mut Vec<u8>,
    requests: &[Request],
) -> io::Result<()> {
    buffer.clear();
    for request in requests {
        request.encode(buffer);
    }
    port.write_all(buffer)
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
        Session::connect(fake, None, 255)
    }

    #[test]
    fn connect_switches_the_device_to_4_000_000_baud() {
        let fake = FakeMakcu::new();
        let (session, info) = connect(&fake).unwrap();
        assert_eq!(info.baud, HIGH_BAUD);
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
        assert!(session.is_connected());
    }

    #[test]
    fn firmware_4075_is_refused_with_the_floor_message() {
        let fake = FakeMakcu::new();
        fake.set_firmware(4075);
        let error = connect(&fake).err().unwrap();
        assert_eq!(
            error.to_string(),
            "MAKCU firmware V4.075 is older than V4.076"
        );
    }

    #[test]
    fn firmware_at_the_floor_and_above_is_accepted() {
        for firmware in [4076, 4094] {
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
        let error = session.request(Request::Interpolate(40)).unwrap_err();
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
        let reply = session.request(Request::Interpolate(255)).unwrap();
        assert_eq!(reply.payload(), [0xFF]);
    }

    #[test]
    fn a_request_after_a_timeout_does_not_use_the_old_connection() {
        let fake = FakeMakcu::new();
        let (session, _) = connect(&fake).unwrap();
        fake.mute_next(0x1F);
        let error = session.request(Request::Interpolate(40)).unwrap_err();
        assert!(matches!(error, SessionError::TimedOut));
        // The late reply has no tag, so the link is down before any other request can run.
        fake.inject(&[0xDE, 0xAD, 1, 0, 0x1F, 17]);
        assert!(!session.is_connected());
        let error = session.request(Request::Interpolate(40)).unwrap_err();
        assert!(matches!(error, SessionError::NotConnected));
        assert!(fake.frames().last().is_some_and(|frame| frame[4] == 0x1F));
    }

    #[test]
    fn a_disconnect_ends_the_session_and_it_stays_down() {
        let fake = FakeMakcu::new();
        let (session, _) = connect(&fake).unwrap();
        fake.disconnect();
        let deadline = Instant::now() + Duration::from_secs(2);
        while session.is_connected() {
            assert!(Instant::now() < deadline, "the session did not end");
            thread::sleep(Duration::from_millis(1));
        }
        fake.reconnect();
        thread::sleep(Duration::from_millis(100));
        assert!(!session.is_connected());
    }
}
