//! The MAKCU wire format and the Raw move ledger. No I/O.
//!
//! Every protocol fact is in `docs/makcu-protocol.md`. The comments here link to its sections.

use std::collections::BTreeMap;

use super::{MakcuRoutes, RawStats};

/// The oldest firmware that the client accepts.
/// See "Firmware version" in `docs/makcu-protocol.md`.
pub(super) const FIRMWARE_FLOOR: u32 = 4085;
/// The timeout of a Raw move, in the unit of `timeout_uf`.
/// See "`timeout_uf`" in `docs/makcu-protocol.md`.
pub(super) const RAW_TIMEOUT_UF: u16 = 800;
/// The number of submits between two collect rounds.
pub(super) const RAW_COLLECT_EVERY: u32 = 8;

const HEADER: [u8; 2] = [0xDE, 0xAD];
/// A packet that declares more payload than this is not a MAKCU packet.
const MAX_PAYLOAD: usize = 64;
/// The ledger forgets the oldest id when it tracks more than this, so that memory stays small
/// when replies do not come. It is more than the record limit in `docs/makcu-protocol.md`.
const MAX_TRACKED: usize = 64;

const CMD_DEVICE: u8 = 0x02;
const CMD_FIRMWARE_VERSION: u8 = 0x04;
const CMD_MOVE: u8 = 0x18;
const CMD_INTERPOLATE: u8 = 0x1F;
const CMD_RAW_MOVE: u8 = 0x69;
const CMD_RAW_STATUS: u8 = 0x6A;
const CMD_SET_BAUD: u8 = 0xA5;

const REASON_UNKNOWN_ID: u8 = 5;
const REASON_TIMED_OUT: u8 = 18;

const RAW_METADATA_SCHEMA: u8 = 1;

/// A request to MAKCU. See "Opcodes" in `docs/makcu-protocol.md`.
/// A `SET` that MAKCU accepts has no reply, except `Interpolate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Request {
    Device,
    FirmwareVersion,
    Move {
        x: i16,
        y: i16,
    },
    Interpolate(u8),
    RawMove {
        id: u32,
        x: i16,
        y: i16,
        timeout_uf: u16,
    },
    /// The result of one Raw move. `id` is at least 1; `id` 0 is `RawMetadata`.
    RawStatus {
        id: u32,
        consume: bool,
    },
    /// The metadata query: `RAW_STATUS` with id 0 and no consume.
    RawMetadata,
    SetBaud(u32),
}

impl Request {
    /// The command byte of the packet. See "Opcodes" in `docs/makcu-protocol.md`.
    pub(super) fn command(&self) -> u8 {
        match self {
            Self::Device => CMD_DEVICE,
            Self::FirmwareVersion => CMD_FIRMWARE_VERSION,
            Self::Move { .. } => CMD_MOVE,
            Self::Interpolate(_) => CMD_INTERPOLATE,
            Self::RawMove { .. } => CMD_RAW_MOVE,
            Self::RawStatus { .. } | Self::RawMetadata => CMD_RAW_STATUS,
            Self::SetBaud(_) => CMD_SET_BAUD,
        }
    }

    /// Appends the packet of the request to `out`.
    pub(super) fn encode(&self, out: &mut Vec<u8>) {
        let len = match self {
            Self::Device | Self::FirmwareVersion => 0,
            Self::Move { .. } => 4,
            Self::Interpolate(_) => 1,
            Self::RawMove { .. } => 10,
            Self::RawStatus { .. } | Self::RawMetadata => 5,
            // This one packet has its own length rule. See "Baud switch" in `docs/makcu-protocol.md`.
            Self::SetBaud(_) => 5,
        };
        out.extend_from_slice(&HEADER);
        out.extend_from_slice(&u16::to_le_bytes(len));
        out.push(self.command());
        match *self {
            Self::Device | Self::FirmwareVersion => {}
            Self::Move { x, y } => {
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
            }
            Self::Interpolate(value) => out.push(value),
            Self::RawMove {
                id,
                x,
                y,
                timeout_uf,
            } => {
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
                out.extend_from_slice(&timeout_uf.to_le_bytes());
            }
            Self::RawStatus { id, consume } => {
                out.extend_from_slice(&id.to_le_bytes());
                out.push(u8::from(consume));
            }
            Self::RawMetadata => out.extend_from_slice(&[0; 5]),
            Self::SetBaud(baud) => out.extend_from_slice(&baud.to_le_bytes()),
        }
    }
}

/// One packet from MAKCU. See "Packet" in `docs/makcu-protocol.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Packet {
    pub cmd: u8,
    pub payload: Vec<u8>,
}

/// A reject has the command of the request and the payload `FF`. See "Reject form" in `docs/makcu-protocol.md`.
/// `INTERPOLATE 255` is the exception: its accepted echo has the same bytes, so it is never a reject.
pub(super) fn is_reject(request: &Request, packet: &Packet) -> bool {
    packet.payload == [0xFF] && *request != Request::Interpolate(255)
}

/// Whether the reply to `INTERPOLATE value` confirms the value. Auto (255) is never checked,
/// because a reject of Auto has the same bytes as its echo.
pub(super) fn is_interpolate_echo(value: u8, packet: &Packet) -> bool {
    value == 255 || packet.payload == [value]
}

/// The replies of the Raw moves go to the ledger, not to a request. See "Reply payload" in `docs/makcu-protocol.md`.
pub(super) fn is_raw_reply(cmd: u8) -> bool {
    matches!(cmd, CMD_RAW_MOVE | CMD_RAW_STATUS)
}

/// Splits a byte stream into packets. Keeps the partial packet between calls.
#[derive(Debug, Default)]
pub(super) struct PacketDecoder {
    buffer: Vec<u8>,
}

impl PacketDecoder {
    pub(super) fn feed(&mut self, bytes: &[u8]) -> Vec<Packet> {
        self.buffer.extend_from_slice(bytes);
        let mut packets = Vec::new();
        let mut start = 0;
        loop {
            let rest = &self.buffer[start..];
            let Some(header) = rest.windows(2).position(|pair| pair == HEADER) else {
                // Keep a lone first header byte: the second one can come in the next call.
                start = self.buffer.len() - usize::from(rest.last() == Some(&HEADER[0]));
                break;
            };
            start += header;
            let rest = &self.buffer[start..];
            if rest.len() < 4 {
                break;
            }
            let len = usize::from(u16::from_le_bytes([rest[2], rest[3]]));
            if len > MAX_PAYLOAD {
                // Not a packet. Skip the header so that garbage cannot stall the stream.
                start += HEADER.len();
                continue;
            }
            let total = 5 + len;
            if rest.len() < total {
                break;
            }
            packets.push(Packet {
                cmd: rest[4],
                payload: rest[5..total].to_vec(),
            });
            start += total;
        }
        self.buffer.drain(..start);
        packets
    }
}

/// The routes in the `DEVICE` reply. See "Opcodes" in `docs/makcu-protocol.md`.
pub(super) fn routes(kinds: u8) -> MakcuRoutes {
    MakcuRoutes {
        mouse: kinds & 0x01 != 0,
    }
}

/// See "Firmware version" in `docs/makcu-protocol.md`.
pub(crate) fn format_firmware(version: u32) -> String {
    format!("V{}.{:03}", version / 1000, version % 1000)
}

/// A short text for a reason code. See "Reply payload" in `docs/makcu-protocol.md`.
pub(crate) fn reject_reason_text(reason: u8) -> &'static str {
    match reason {
        1 => "invalid length, id, consume flag, or timeout",
        2 => "id not above the highest accepted id",
        3 => "no room in the records or the command queue",
        4 => "mouse target not available",
        5 => "unknown id, or result already consumed",
        6 => "consume of a queued request",
        7 => "firmware update or restart in progress",
        17 => "range or descriptor check failed",
        18 => "timed out",
        20 => "target lost before the submit",
        21 => "endpoint reset",
        _ => "unknown reason",
    }
}

/// The reply of the metadata query. See "`RAW_STATUS`" in `docs/makcu-protocol.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RawMetadata {
    pub last_accepted_id: u32,
    pub free_records: u8,
    pub capacity: u8,
}

pub(super) fn parse_raw_metadata(payload: &[u8]) -> Option<RawMetadata> {
    let [0, RAW_METADATA_SCHEMA, a, b, c, d, free_records, capacity] = *payload else {
        return None;
    };
    Some(RawMetadata {
        last_accepted_id: u32::from_le_bytes([a, b, c, d]),
        free_records,
        capacity,
    })
}

/// The state in a reply of an id. See "Reply payload" in `docs/makcu-protocol.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RawState {
    Queued,
    Submitted,
    Rejected,
    ImmediateError,
}

impl RawState {
    fn from_wire(state: u8) -> Option<Self> {
        match state {
            1 => Some(Self::Queued),
            2 => Some(Self::Submitted),
            3 => Some(Self::Rejected),
            255 => Some(Self::ImmediateError),
            _ => None,
        }
    }
}

/// A reply of `RAW_MOVE` or of `RAW_STATUS` with an id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RawReply {
    Submit {
        id: u32,
        state: RawState,
        reason: u8,
    },
    Status {
        id: u32,
        state: RawState,
        reason: u8,
    },
}

/// Reads the reply of an id. The metadata reply is not one, and gives `None`.
pub(super) fn parse_raw_reply(packet: &Packet) -> Option<RawReply> {
    let [state, reason, a, b, c, d] = *packet.payload.as_slice() else {
        return None;
    };
    let state = RawState::from_wire(state)?;
    let id = u32::from_le_bytes([a, b, c, d]);
    match packet.cmd {
        CMD_RAW_MOVE => Some(RawReply::Submit { id, state, reason }),
        CMD_RAW_STATUS => Some(RawReply::Status { id, state, reason }),
        _ => None,
    }
}

/// Tracks the Raw moves that MAKCU has not yet reported.
///
/// It does no I/O. The client sends the requests that it returns and gives it the replies.
#[derive(Debug)]
pub(super) struct RawLedger {
    /// The id of the next submit. `None` when the ids ran out.
    next_id: Option<u32>,
    newest_id: u32,
    submits_since_collect: u32,
    /// The ids that wait for a result. The value is true when the result is final
    /// and the id waits for its consume.
    pending: BTreeMap<u32, bool>,
    stats: RawStats,
}

impl RawLedger {
    /// `first_id` is `last_accepted_id + 1`. An id is never zero.
    pub(super) fn new(first_id: u32) -> Self {
        Self {
            next_id: Some(first_id.max(1)),
            newest_id: 0,
            submits_since_collect: 0,
            pending: BTreeMap::new(),
            stats: RawStats::default(),
        }
    }

    /// Takes the next id. Returns `None` when the ids ran out.
    pub(super) fn next_submit(&mut self, x: i16, y: i16) -> Option<Request> {
        let id = self.next_id?;
        self.next_id = id.checked_add(1);
        self.newest_id = id;
        self.submits_since_collect += 1;
        self.stats.submitted += 1;
        self.pending.insert(id, false);
        if self.pending.len() > MAX_TRACKED {
            self.pending.pop_first();
        }
        Some(Request::RawMove {
            id,
            x,
            y,
            timeout_uf: RAW_TIMEOUT_UF,
        })
    }

    pub(super) fn on_reply(&mut self, reply: RawReply) {
        match reply {
            RawReply::Submit { id, state, reason } => match state {
                RawState::Queued => {}
                RawState::Submitted | RawState::Rejected => self.finish(id, state, reason),
                // MAKCU did not accept the id. It made no record, so nothing waits.
                RawState::ImmediateError => {
                    self.pending.remove(&id);
                    self.count_reject(reason);
                }
            },
            RawReply::Status { id, state, reason } => match state {
                RawState::Submitted | RawState::Rejected => self.finish(id, state, reason),
                RawState::ImmediateError if reason == REASON_UNKNOWN_ID => {
                    self.pending.remove(&id);
                }
                RawState::Queued | RawState::ImmediateError => {}
            },
        }
    }

    /// The `RAW_STATUS` requests that are due now. Empty most of the time.
    ///
    /// A final result gets its consume. Every `RAW_COLLECT_EVERY` submits, each older
    /// id that is not final gets a peek.
    pub(super) fn due_requests(&mut self) -> Vec<Request> {
        let mut due = Vec::new();
        self.pending.retain(|&id, &mut is_final| {
            if is_final {
                due.push(Request::RawStatus { id, consume: true });
            }
            !is_final
        });
        if self.submits_since_collect >= RAW_COLLECT_EVERY {
            self.submits_since_collect = 0;
            due.extend(
                self.pending
                    .keys()
                    .filter(|&&id| id < self.newest_id)
                    .map(|&id| Request::RawStatus { id, consume: false }),
            );
        }
        due
    }

    pub(super) fn stats(&self) -> RawStats {
        self.stats
    }

    /// Records the final result of an id. A result of an unknown or final id is ignored.
    fn finish(&mut self, id: u32, state: RawState, reason: u8) {
        let Some(is_final) = self.pending.get_mut(&id) else {
            return;
        };
        if *is_final {
            return;
        }
        *is_final = true;
        if state == RawState::Submitted {
            self.stats.consecutive_rejected = 0;
        } else {
            self.count_reject(reason);
        }
    }

    fn count_reject(&mut self, reason: u8) {
        self.stats.rejected += 1;
        self.stats.consecutive_rejected += 1;
        self.stats.last_reason = Some(reason);
        if reason == REASON_TIMED_OUT {
            self.stats.timed_out += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(request: Request) -> Vec<u8> {
        let mut out = Vec::new();
        request.encode(&mut out);
        out
    }

    fn packet(cmd: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0xDE, 0xAD, payload.len() as u8, 0, cmd];
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn requests_encode_to_the_documented_bytes() {
        let cases: Vec<(Request, Vec<u8>)> = vec![
            (Request::Device, vec![0xDE, 0xAD, 0, 0, 0x02]),
            (Request::FirmwareVersion, vec![0xDE, 0xAD, 0, 0, 0x04]),
            (
                Request::Move { x: 10, y: -5 },
                vec![0xDE, 0xAD, 4, 0, 0x18, 0x0A, 0, 0xFB, 0xFF],
            ),
            (
                Request::Interpolate(255),
                vec![0xDE, 0xAD, 1, 0, 0x1F, 0xFF],
            ),
            (Request::Interpolate(40), vec![0xDE, 0xAD, 1, 0, 0x1F, 0x28]),
            (
                Request::RawMove {
                    id: 1,
                    x: 100,
                    y: 0,
                    timeout_uf: 50,
                },
                vec![
                    0xDE, 0xAD, 0x0A, 0, 0x69, 1, 0, 0, 0, 0x64, 0, 0, 0, 0x32, 0,
                ],
            ),
            (
                Request::RawMove {
                    id: 1,
                    x: 5,
                    y: -5,
                    timeout_uf: RAW_TIMEOUT_UF,
                },
                vec![
                    0xDE, 0xAD, 0x0A, 0, 0x69, 1, 0, 0, 0, 5, 0, 0xFB, 0xFF, 0x20, 0x03,
                ],
            ),
            (
                Request::RawStatus {
                    id: 1,
                    consume: false,
                },
                vec![0xDE, 0xAD, 5, 0, 0x6A, 1, 0, 0, 0, 0],
            ),
            (
                Request::RawStatus {
                    id: 1,
                    consume: true,
                },
                vec![0xDE, 0xAD, 5, 0, 0x6A, 1, 0, 0, 0, 1],
            ),
            (
                Request::RawMetadata,
                vec![0xDE, 0xAD, 5, 0, 0x6A, 0, 0, 0, 0, 0],
            ),
            (
                Request::SetBaud(4_000_000),
                vec![0xDE, 0xAD, 5, 0, 0xA5, 0x00, 0x09, 0x3D, 0x00],
            ),
            (
                Request::SetBaud(115_200),
                vec![0xDE, 0xAD, 5, 0, 0xA5, 0x00, 0xC2, 0x01, 0x00],
            ),
        ];
        for (request, bytes) in cases {
            assert_eq!(encoded(request), bytes, "{request:?}");
        }
    }

    #[test]
    fn encode_appends_to_the_buffer() {
        let mut out = vec![7];
        Request::Device.encode(&mut out);
        assert_eq!(out, [7, 0xDE, 0xAD, 0, 0, 0x02]);
    }

    #[test]
    fn decoder_returns_joined_packets() {
        let mut bytes = packet(0x04, &[0xEE, 0x0F, 0, 0]);
        bytes.extend(packet(0x1F, &[0xFF]));
        let packets = PacketDecoder::default().feed(&bytes);
        assert_eq!(
            packets,
            [
                Packet {
                    cmd: 0x04,
                    payload: vec![0xEE, 0x0F, 0, 0]
                },
                Packet {
                    cmd: 0x1F,
                    payload: vec![0xFF]
                },
            ]
        );
    }

    #[test]
    fn decoder_reads_a_packet_split_at_every_byte() {
        let bytes = packet(0x04, &[0xEE, 0x0F, 0, 0]);
        let mut decoder = PacketDecoder::default();
        let mut packets = Vec::new();
        for byte in &bytes {
            packets.extend(decoder.feed(&[*byte]));
        }
        assert_eq!(
            packets,
            [Packet {
                cmd: 0x04,
                payload: vec![0xEE, 0x0F, 0, 0]
            }]
        );
    }

    #[test]
    fn decoder_skips_garbage_before_the_header() {
        let mut bytes = vec![0x00, 0xDE, 0x13, 0xAD];
        bytes.extend(packet(0x02, &[1]));
        let packets = PacketDecoder::default().feed(&bytes);
        assert_eq!(
            packets,
            [Packet {
                cmd: 0x02,
                payload: vec![1]
            }]
        );
    }

    #[test]
    fn decoder_keeps_a_partial_packet_between_calls() {
        let bytes = packet(0x02, &[1]);
        let mut decoder = PacketDecoder::default();
        assert!(decoder.feed(&bytes[..3]).is_empty());
        assert_eq!(decoder.feed(&bytes[3..]).len(), 1);
        assert!(decoder.feed(&[]).is_empty());
    }

    #[test]
    fn decoder_drops_a_packet_with_an_impossible_length() {
        let mut bytes = vec![0xDE, 0xAD, 0xFF, 0xFF, 0x02];
        bytes.extend(packet(0x04, &[1, 2, 3, 4]));
        let packets = PacketDecoder::default().feed(&bytes);
        assert_eq!(
            packets,
            [Packet {
                cmd: 0x04,
                payload: vec![1, 2, 3, 4]
            }]
        );
    }

    #[test]
    fn decoder_reads_a_reject_packet() {
        let packets = PacketDecoder::default().feed(&[0xDE, 0xAD, 1, 0, 0x04, 0xFF]);
        assert_eq!(
            packets,
            [Packet {
                cmd: 0x04,
                payload: vec![0xFF]
            }]
        );
    }

    #[test]
    fn firmware_is_formatted_with_three_build_digits() {
        assert_eq!(format_firmware(4085), "V4.085");
        assert_eq!(format_firmware(4094), "V4.094");
    }

    #[test]
    fn only_bit_zero_is_the_mouse_route() {
        assert!(routes(0x01).mouse);
        assert!(routes(0x03).mouse);
        assert!(!routes(0x02).mouse);
        assert!(!routes(0).mouse);
    }

    #[test]
    fn reject_reasons_have_a_text() {
        assert_eq!(reject_reason_text(18), "timed out");
        assert_eq!(reject_reason_text(99), "unknown reason");
    }

    #[test]
    fn metadata_is_parsed_from_the_documented_payload() {
        let payload = [0, 1, 1, 0, 0, 0, 0x10, 0x10];
        assert_eq!(
            parse_raw_metadata(&payload),
            Some(RawMetadata {
                last_accepted_id: 1,
                free_records: 16,
                capacity: 16,
            })
        );
        assert_eq!(parse_raw_metadata(&payload[..7]), None);
        assert_eq!(parse_raw_metadata(&[0, 2, 1, 0, 0, 0, 16, 16]), None);
        assert_eq!(parse_raw_metadata(&[1, 1, 1, 0, 0, 0, 16, 16]), None);
    }

    #[test]
    fn raw_replies_are_parsed_by_command() {
        let reply = |cmd, payload: &[u8]| {
            parse_raw_reply(&Packet {
                cmd,
                payload: payload.to_vec(),
            })
        };
        assert_eq!(
            reply(0x69, &[1, 0, 1, 0, 0, 0]),
            Some(RawReply::Submit {
                id: 1,
                state: RawState::Queued,
                reason: 0
            })
        );
        assert_eq!(
            reply(0x6A, &[3, 0x12, 7, 0, 0, 0]),
            Some(RawReply::Status {
                id: 7,
                state: RawState::Rejected,
                reason: 18
            })
        );
        assert_eq!(reply(0x6A, &[0, 1, 1, 0, 0, 0, 16, 16]), None);
        assert_eq!(reply(0x6A, &[9, 0, 1, 0, 0, 0]), None);
        assert_eq!(reply(0x18, &[1, 0, 1, 0, 0, 0]), None);
    }

    fn raw_move_id(request: Request) -> u32 {
        match request {
            Request::RawMove { id, .. } => id,
            other => panic!("not a raw move: {other:?}"),
        }
    }

    fn submit(ledger: &mut RawLedger, count: u32) {
        for _ in 0..count {
            ledger.next_submit(1, 1).unwrap();
        }
    }

    fn status(id: u32, state: RawState, reason: u8) -> RawReply {
        RawReply::Status { id, state, reason }
    }

    #[test]
    fn ledger_submits_grow_from_the_first_id() {
        let mut ledger = RawLedger::new(10);
        let request = ledger.next_submit(5, -5).unwrap();
        assert_eq!(
            request,
            Request::RawMove {
                id: 10,
                x: 5,
                y: -5,
                timeout_uf: RAW_TIMEOUT_UF
            }
        );
        assert_eq!(raw_move_id(ledger.next_submit(0, 0).unwrap()), 11);
        assert_eq!(ledger.stats().submitted, 2);
    }

    #[test]
    fn ledger_collects_only_after_eight_submits() {
        let mut ledger = RawLedger::new(1);
        submit(&mut ledger, 7);
        assert!(ledger.due_requests().is_empty());
        submit(&mut ledger, 1);
        let due = ledger.due_requests();
        let expected: Vec<Request> = (1..=7)
            .map(|id| Request::RawStatus { id, consume: false })
            .collect();
        assert_eq!(due, expected);
        assert!(ledger.due_requests().is_empty());
    }

    #[test]
    fn ledger_consumes_a_final_id_after_the_peek() {
        let mut ledger = RawLedger::new(1);
        submit(&mut ledger, 8);
        ledger.due_requests();
        ledger.on_reply(status(1, RawState::Submitted, 0));
        ledger.on_reply(status(2, RawState::Rejected, 17));
        assert_eq!(
            ledger.due_requests(),
            [
                Request::RawStatus {
                    id: 1,
                    consume: true
                },
                Request::RawStatus {
                    id: 2,
                    consume: true
                },
            ]
        );
        assert!(ledger.due_requests().is_empty());
    }

    #[test]
    fn ledger_peeks_a_queued_id_again_in_the_next_round() {
        let mut ledger = RawLedger::new(1);
        submit(&mut ledger, 8);
        ledger.due_requests();
        ledger.on_reply(status(1, RawState::Queued, 0));
        submit(&mut ledger, 8);
        let due = ledger.due_requests();
        assert!(due.contains(&Request::RawStatus {
            id: 1,
            consume: false
        }));
        assert!(
            !due.iter()
                .any(|request| matches!(request, Request::RawStatus { consume: true, .. }))
        );
    }

    #[test]
    fn ledger_resets_consecutive_rejects_on_a_submitted_result() {
        let mut ledger = RawLedger::new(1);
        submit(&mut ledger, 3);
        ledger.on_reply(status(1, RawState::Rejected, 17));
        ledger.on_reply(status(2, RawState::Rejected, 17));
        assert_eq!(ledger.stats().consecutive_rejected, 2);
        ledger.on_reply(status(3, RawState::Submitted, 0));
        let stats = ledger.stats();
        assert_eq!(stats.consecutive_rejected, 0);
        assert_eq!(stats.rejected, 2);
        assert_eq!(stats.last_reason, Some(17));
    }

    #[test]
    fn ledger_counts_immediate_errors_as_rejected() {
        let mut ledger = RawLedger::new(1);
        submit(&mut ledger, 2);
        ledger.on_reply(RawReply::Submit {
            id: 1,
            state: RawState::ImmediateError,
            reason: 3,
        });
        ledger.on_reply(RawReply::Submit {
            id: 2,
            state: RawState::Rejected,
            reason: 17,
        });
        let stats = ledger.stats();
        assert_eq!(stats.rejected, 2);
        assert_eq!(stats.consecutive_rejected, 2);
        assert_eq!(stats.timed_out, 0);
    }

    #[test]
    fn ledger_does_not_wait_for_an_id_that_was_not_accepted() {
        let mut ledger = RawLedger::new(1);
        submit(&mut ledger, 9);
        ledger.on_reply(RawReply::Submit {
            id: 1,
            state: RawState::ImmediateError,
            reason: 3,
        });
        let due = ledger.due_requests();
        assert!(
            !due.iter()
                .any(|request| matches!(request, Request::RawStatus { id: 1, .. }))
        );
    }

    #[test]
    fn ledger_counts_reason_eighteen_as_timed_out() {
        let mut ledger = RawLedger::new(1);
        submit(&mut ledger, 1);
        ledger.on_reply(status(1, RawState::Rejected, 18));
        let stats = ledger.stats();
        assert_eq!(stats.timed_out, 1);
        assert_eq!(stats.rejected, 1);
    }

    #[test]
    fn ledger_counts_a_result_once() {
        let mut ledger = RawLedger::new(1);
        submit(&mut ledger, 1);
        ledger.on_reply(status(1, RawState::Rejected, 17));
        ledger.on_reply(status(1, RawState::Rejected, 17));
        ledger.on_reply(status(9, RawState::Rejected, 17));
        assert_eq!(ledger.stats().rejected, 1);
    }

    #[test]
    fn ledger_burns_a_rejected_id_and_never_resends() {
        let mut ledger = RawLedger::new(1);
        let first = raw_move_id(ledger.next_submit(1, 1).unwrap());
        ledger.on_reply(RawReply::Submit {
            id: first,
            state: RawState::ImmediateError,
            reason: 3,
        });
        let second = raw_move_id(ledger.next_submit(2, 2).unwrap());
        assert_eq!((first, second), (1, 2));
        assert!(ledger.due_requests().is_empty());
    }

    #[test]
    fn ledger_runs_out_of_ids_at_the_largest_id() {
        let mut ledger = RawLedger::new(u32::MAX - 1);
        assert_eq!(raw_move_id(ledger.next_submit(1, 1).unwrap()), u32::MAX - 1);
        assert_eq!(raw_move_id(ledger.next_submit(1, 1).unwrap()), u32::MAX);
        assert!(ledger.next_submit(1, 1).is_none());
    }

    #[test]
    fn ledger_forgets_consumed_ids() {
        let mut ledger = RawLedger::new(1);
        for round in 0..20 {
            submit(&mut ledger, 8);
            for id in (round * 8 + 1)..=(round * 8 + 8) {
                ledger.on_reply(status(id, RawState::Submitted, 0));
            }
            ledger.due_requests();
        }
        assert!(ledger.pending.is_empty());
    }
}
