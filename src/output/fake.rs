use std::{collections::VecDeque, io, sync::Arc, sync::Mutex, thread, time::Duration};

use super::{
    Port as Entry,
    port::{Connector, Serial},
    protocol::{CMD_DEVICE, CMD_FIRMWARE_VERSION, CMD_INTERPOLATE, CMD_SET_BAUD, HEADER, REJECT},
};
use crate::sync::MutexExt;

const PORT_NAME: &str = "COM3";

#[derive(Clone)]
pub(crate) struct FakeMakcu {
    state: Arc<Mutex<State>>,
}

struct State {
    baud: u32,
    accepted: Vec<u32>,
    firmware: u32,
    kinds: u8,
    connected: bool,
    frames: Vec<Vec<u8>>,
    replies: VecDeque<u8>,
    mute: Option<u8>,
    reject: Option<u8>,
    interpolate_echo: Option<u8>,
}

fn unplugged() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "MAKCU is unplugged")
}

impl FakeMakcu {
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
                interpolate_echo: None,
            })),
        }
    }

    pub(crate) fn frames(&self) -> Vec<Vec<u8>> {
        self.state.lock_recover().frames.clone()
    }

    pub(crate) fn disconnect(&self) {
        let mut state = self.state.lock_recover();
        state.connected = false;
        state.replies.clear();
    }

    pub(crate) fn reconnect(&self) {
        self.state.lock_recover().connected = true;
    }

    pub(crate) fn set_firmware(&self, firmware: u32) {
        self.state.lock_recover().firmware = firmware;
    }

    pub(crate) fn set_kinds(&self, kinds: u8) {
        self.state.lock_recover().kinds = kinds;
    }

    pub(crate) fn set_baud(&self, baud: u32) {
        self.state.lock_recover().baud = baud;
    }

    pub(crate) fn set_accepted_bauds(&self, bauds: &[u32]) {
        self.state.lock_recover().accepted = bauds.to_vec();
    }

    pub(crate) fn mute_next(&self, cmd: u8) {
        self.state.lock_recover().mute = Some(cmd);
    }

    pub(crate) fn reject_next(&self, cmd: u8) {
        self.state.lock_recover().reject = Some(cmd);
    }

    pub(crate) fn set_interpolate_echo(&self, value: u8) {
        self.state.lock_recover().interpolate_echo = Some(value);
    }
}

impl Connector for FakeMakcu {
    fn ports(&self) -> io::Result<Vec<Entry>> {
        Ok(vec![Entry {
            name: PORT_NAME.to_owned(),
            description: "MAKCU".to_owned(),
        }])
    }

    fn open(&self, port: &str, baud: u32) -> io::Result<Box<dyn Serial>> {
        if port != PORT_NAME {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such port"));
        }
        let mut state = self.state.lock_recover();
        if !state.connected {
            return Err(unplugged());
        }
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

impl Serial for FakePort {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut state = self.state.lock_recover();
        if !state.connected {
            return Err(unplugged());
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
        let mut state = self.state.lock_recover();
        if !state.connected {
            return Err(unplugged());
        }
        for query in split_requests(bytes) {
            state.frames.push(query.to_vec());
            if self.baud == state.baud {
                state.answer(query[4], &query[5..]);
            }
        }
        Ok(())
    }

    fn check(&mut self) -> io::Result<()> {
        if self.state.lock_recover().connected {
            Ok(())
        } else {
            Err(unplugged())
        }
    }
}

impl State {
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
            CMD_DEVICE => {
                let kinds = self.kinds;
                self.reply(cmd, &[kinds]);
            }
            CMD_FIRMWARE_VERSION => {
                let firmware = self.firmware.to_le_bytes();
                self.reply(cmd, &firmware);
            }
            CMD_INTERPOLATE => {
                let echo = self
                    .interpolate_echo
                    .map_or_else(|| payload.to_vec(), |value| vec![value]);
                self.reply(cmd, &echo);
            }
            CMD_SET_BAUD => {
                let baud = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                if self.accepted.contains(&baud) {
                    self.baud = baud;
                } else {
                    self.reject_with(cmd);
                }
            }
            _ => {}
        }
    }

    fn reply(&mut self, cmd: u8, payload: &[u8]) {
        let len = payload.len() as u8;
        self.replies
            .extend(HEADER.iter().chain(&[len, 0, cmd]).chain(payload));
    }

    fn reject_with(&mut self, cmd: u8) {
        self.reply(cmd, &[REJECT]);
    }
}

/// A `SET_BAUD` packet is 9 bytes whatever its length byte says.
fn split_requests(mut bytes: &[u8]) -> Vec<&[u8]> {
    let mut queries = Vec::new();
    while bytes.len() >= 5 {
        let total = if bytes[4] == CMD_SET_BAUD {
            9
        } else {
            5 + usize::from(u16::from_le_bytes([bytes[2], bytes[3]]))
        };
        let total = total.min(bytes.len());
        queries.push(&bytes[..total]);
        bytes = &bytes[total..];
    }
    queries
}
