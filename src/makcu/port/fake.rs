//! A MAKCU in memory, for the tests of the session.
//!
//! Each write of a port is one request, as the session sends it. The fake answers the requests
//! that the session needs, and it can be made to fail the way the device fails.

use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    thread,
    time::Duration,
};

use super::{Connector, Port};

const PORT_NAME: &str = "COM3";

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
    /// Every write of every port, in order.
    frames: Vec<Vec<u8>>,
    /// The bytes that wait for the host.
    replies: VecDeque<u8>,
    /// The command whose next request gets no reply.
    mute: Option<u8>,
    /// The command whose next request gets a reject.
    reject: Option<u8>,
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
                replies: VecDeque::new(),
                mute: None,
                reject: None,
            })),
        }
    }

    pub(crate) fn frames(&self) -> Vec<Vec<u8>> {
        lock(&self.state).frames.clone()
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
        state.frames.push(bytes.to_vec());
        // A device does not hear a host at another rate.
        if self.baud == state.baud {
            state.answer(bytes[4], &bytes[5..]);
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
            0x1F => self.reply(cmd, payload),
            // The metadata is for id 0. Any other id is rejected.
            0x6A if payload[..4] == [0, 0, 0, 0] => self.reply(cmd, &[0, 1, 0, 0, 0, 0, 16, 16]),
            0x6A => self.reject_with(cmd),
            0xA5 => {
                let baud = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                if self.accepted.contains(&baud) {
                    // A `SetBaud` that is accepted gets no reply.
                    self.baud = baud;
                } else {
                    self.reject_with(cmd);
                }
            }
            // The moves are not answered yet.
            _ => {}
        }
    }

    fn reply(&mut self, cmd: u8, payload: &[u8]) {
        let len = payload.len() as u8;
        self.replies
            .extend([0xDE, 0xAD, len, 0, cmd].iter().chain(payload));
    }

    fn reject_with(&mut self, cmd: u8) {
        self.replies.extend([0xDE, 0xAD, 1, 0, cmd, 0xFF]);
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}
