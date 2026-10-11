//! A MAKCU in memory, for the tests of the session.
//!
//! Each write of a port is one request, as the session sends it. The fake answers the requests
//! that the session needs, and it can be made to fail the way the device fails.

use std::{
    collections::{BTreeMap, VecDeque},
    io,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    thread,
    time::Duration,
};

use super::{Connector, Port};

const PORT_NAME: &str = "COM3";

/// The state that a Raw move reply gives: `(state, reason)`. See "Reply payload" in `docs/makcu-protocol.md`.
type RawAnswer = (u8, u8);

#[derive(Clone)]
pub(crate) struct FakeMakcu {
    state: Arc<Mutex<State>>,
}

struct State {
    /// The rate that the device listens at. A port at another rate gets no reply.
    baud: u32,
    /// The rates that a `SetBaud` switches to. Another rate is rejected.
    accepted: Vec<u32>,
    firmware: u32,
    kinds: u8,
    connected: bool,
    /// Every request that the device received, in order. One port write can hold several.
    frames: Vec<Vec<u8>>,
    /// Every port write, in order, as the bytes of that write.
    writes: Vec<Vec<u8>>,
    /// The bytes that wait for the host.
    replies: VecDeque<u8>,
    /// The command whose next request gets no reply.
    mute: Option<u8>,
    /// The command whose next request gets a reject.
    reject: Option<u8>,
    /// The answer to a `RawMove` of each id. An id without one is queued.
    raw_submit: BTreeMap<u32, RawAnswer>,
    /// The answer to a `RawStatus` of each id. An id without one is submitted.
    raw_status: BTreeMap<u32, RawAnswer>,
    /// The `last_accepted_id` of the metadata reply.
    raw_last_accepted: u32,
    /// Whether the firmware answers the metadata query. When `false`, it rejects it.
    raw_metadata: bool,
    /// The value that `INTERPOLATE` echoes instead of its own. `None` echoes the value sent.
    interpolate_echo: Option<u8>,
}

impl FakeMakcu {
    /// A MAKCU at 115,200 baud with firmware V4.094, a mouse route, and both rates accepted.
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                baud: 115_200,
                accepted: vec![115_200, 4_000_000],
                firmware: 4094,
                kinds: 0x01,
                connected: true,
                frames: Vec::new(),
                writes: Vec::new(),
                replies: VecDeque::new(),
                mute: None,
                reject: None,
                raw_submit: BTreeMap::new(),
                raw_status: BTreeMap::new(),
                raw_last_accepted: 0,
                raw_metadata: true,
                interpolate_echo: None,
            })),
        }
    }

    /// Every request that the device received, in order. One port write can hold several.
    pub(crate) fn frames(&self) -> Vec<Vec<u8>> {
        lock(&self.state).frames.clone()
    }

    /// Every port write, in order, as the bytes of that write.
    pub(crate) fn writes(&self) -> Vec<Vec<u8>> {
        lock(&self.state).writes.clone()
    }

    /// The cable is pulled. Reads and writes fail until `reconnect`.
    pub(crate) fn disconnect(&self) {
        let mut state = lock(&self.state);
        state.connected = false;
        state.replies.clear();
    }

    pub(crate) fn reconnect(&self) {
        lock(&self.state).connected = true;
    }

    pub(crate) fn set_firmware(&self, firmware: u32) {
        lock(&self.state).firmware = firmware;
    }

    pub(crate) fn set_kinds(&self, kinds: u8) {
        lock(&self.state).kinds = kinds;
    }

    /// The rate that the device listens at now.
    pub(crate) fn set_baud(&self, baud: u32) {
        lock(&self.state).baud = baud;
    }

    pub(crate) fn set_accepted_bauds(&self, bauds: &[u32]) {
        lock(&self.state).accepted = bauds.to_vec();
    }

    /// The next request of `cmd` gets no reply.
    pub(crate) fn mute_next(&self, cmd: u8) {
        lock(&self.state).mute = Some(cmd);
    }

    /// The next request of `cmd` gets a reject.
    pub(crate) fn reject_next(&self, cmd: u8) {
        lock(&self.state).reject = Some(cmd);
    }

    /// The device sends these bytes now.
    pub(crate) fn inject(&self, bytes: &[u8]) {
        lock(&self.state).replies.extend(bytes);
    }

    /// The reply of `RawMove` for `id`: `state` and `reason`.
    pub(crate) fn set_raw_submit(&self, id: u32, state: u8, reason: u8) {
        lock(&self.state).raw_submit.insert(id, (state, reason));
    }

    /// The reply of `RawStatus` for `id`: `state` and `reason`.
    pub(crate) fn set_raw_status(&self, id: u32, state: u8, reason: u8) {
        lock(&self.state).raw_status.insert(id, (state, reason));
    }

    pub(crate) fn set_raw_last_accepted(&self, id: u32) {
        lock(&self.state).raw_last_accepted = id;
    }

    /// `INTERPOLATE` echoes `value` instead of the value it was sent.
    pub(crate) fn set_interpolate_echo(&self, value: u8) {
        lock(&self.state).interpolate_echo = Some(value);
    }

    /// `false` makes the firmware reject the metadata query.
    pub(crate) fn set_raw_metadata(&self, answered: bool) {
        lock(&self.state).raw_metadata = answered;
    }
}

impl Connector for FakeMakcu {
    fn ports(&self) -> io::Result<Vec<String>> {
        Ok(vec![PORT_NAME.to_owned()])
    }

    fn open(&self, port: &str, baud: u32) -> io::Result<Box<dyn Port>> {
        if port != PORT_NAME {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such port"));
        }
        let mut state = lock(&self.state);
        if !state.connected {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "MAKCU is unplugged",
            ));
        }
        // Like `PurgeComm` on open: the bytes of the old connection are gone.
        state.replies.clear();
        Ok(Box::new(FakePort {
            state: Arc::clone(&self.state),
            baud,
        }))
    }
}

struct FakePort {
    state: Arc<Mutex<State>>,
    baud: u32,
}

impl Port for FakePort {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut state = lock(&self.state);
        if !state.connected {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "MAKCU is unplugged",
            ));
        }
        if self.baud != state.baud || state.replies.is_empty() {
            drop(state);
            thread::sleep(Duration::from_millis(1));
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no reply from MAKCU",
            ));
        }
        let count = buf.len().min(state.replies.len());
        for slot in &mut buf[..count] {
            *slot = state.replies.pop_front().unwrap_or_default();
        }
        Ok(count)
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut state = lock(&self.state);
        if !state.connected {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "MAKCU is unplugged",
            ));
        }
        state.writes.push(bytes.to_vec());
        for request in split_requests(bytes) {
            state.frames.push(request.to_vec());
            // A device does not hear a host at another rate.
            if self.baud == state.baud {
                state.answer(request[4], &request[5..]);
            }
        }
        Ok(())
    }

    fn try_clone(&self) -> io::Result<Box<dyn Port>> {
        Ok(Box::new(Self {
            state: Arc::clone(&self.state),
            baud: self.baud,
        }))
    }
}

impl State {
    /// Answers one request. `payload` is the payload of the request, `cmd` its command.
    fn answer(&mut self, cmd: u8, payload: &[u8]) {
        if self.mute == Some(cmd) {
            self.mute = None;
            return;
        }
        if self.reject == Some(cmd) {
            self.reject = None;
            return self.reject_with(cmd);
        }
        match cmd {
            0x02 => {
                let kinds = self.kinds;
                self.reply(cmd, &[kinds]);
            }
            0x04 => {
                let firmware = self.firmware.to_le_bytes();
                self.reply(cmd, &firmware);
            }
            // `INTERPOLATE` echoes its value.
            0x1F => {
                let echo = self
                    .interpolate_echo
                    .map_or_else(|| payload.to_vec(), |value| vec![value]);
                self.reply(cmd, &echo);
            }
            // `RAW_MOVE` replies with the state of its id.
            0x69 => {
                let id = id_of(payload);
                let (state, reason) = self.raw_submit.get(&id).copied().unwrap_or((1, 0));
                self.raw_reply(cmd, state, reason, id);
            }
            // The metadata is for id 0. Its reply is `last_accepted_id`, then 16 free records of 16.
            0x6A if id_of(payload) == 0 => {
                if self.raw_metadata {
                    let last = self.raw_last_accepted.to_le_bytes();
                    self.reply(cmd, &[0, 1, last[0], last[1], last[2], last[3], 16, 16]);
                } else {
                    self.reject_with(cmd);
                }
            }
            // `RAW_STATUS` with an id replies with the state of that id.
            0x6A => {
                let id = id_of(payload);
                let (state, reason) = self.raw_status.get(&id).copied().unwrap_or((2, 0));
                self.raw_reply(cmd, state, reason, id);
            }
            0xA5 => {
                let baud = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                if self.accepted.contains(&baud) {
                    // A `SetBaud` that is accepted gets no reply.
                    self.baud = baud;
                } else {
                    self.reject_with(cmd);
                }
            }
            // The moves are not answered.
            _ => {}
        }
    }

    fn reply(&mut self, cmd: u8, payload: &[u8]) {
        let len = payload.len() as u8;
        self.replies
            .extend([0xDE, 0xAD, len, 0, cmd].iter().chain(payload));
    }

    /// A reply of a Raw move: the state, the reason, and the id.
    fn raw_reply(&mut self, cmd: u8, state: u8, reason: u8, id: u32) {
        let id = id.to_le_bytes();
        self.reply(cmd, &[state, reason, id[0], id[1], id[2], id[3]]);
    }

    fn reject_with(&mut self, cmd: u8) {
        self.replies.extend([0xDE, 0xAD, 1, 0, cmd, 0xFF]);
    }
}

/// The requests in one port write. A `SET_BAUD` packet is 9 bytes whatever its length byte says.
/// See "Baud switch" in `docs/makcu-protocol.md`.
fn split_requests(mut bytes: &[u8]) -> Vec<&[u8]> {
    const CMD_SET_BAUD: u8 = 0xA5;
    let mut requests = Vec::new();
    while bytes.len() >= 5 {
        let total = if bytes[4] == CMD_SET_BAUD {
            9
        } else {
            5 + usize::from(u16::from_le_bytes([bytes[2], bytes[3]]))
        };
        let total = total.min(bytes.len());
        requests.push(&bytes[..total]);
        bytes = &bytes[total..];
    }
    requests
}

/// The id at the start of a Raw request payload.
fn id_of(payload: &[u8]) -> u32 {
    u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]])
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}
