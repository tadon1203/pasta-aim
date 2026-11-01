//! The MAKCU wire format. No I/O.
//!
//! Every protocol fact is in `docs/makcu-protocol.md`. The comments here link to its sections.

/// The wire value of `INTERPOLATE` for Auto.
pub(super) const INTERPOLATE_AUTO: u8 = 255;

pub(super) const HEADER: [u8; 2] = [0xDE, 0xAD];
/// A packet that declares more payload than this is not a MAKCU packet.
pub(super) const MAX_PAYLOAD: usize = 64;

pub(super) const CMD_DEVICE: u8 = 0x02;
pub(super) const CMD_FIRMWARE_VERSION: u8 = 0x04;
pub(super) const CMD_MOVE: u8 = 0x18;
pub(super) const CMD_INTERPOLATE: u8 = 0x1F;
pub(super) const CMD_SET_BAUD: u8 = 0xA5;

/// The bit of the `DEVICE` reply that means the mouse route is up. See "Opcodes" in `docs/makcu-protocol.md`.
pub(super) const KIND_MOUSE: u8 = 0x01;

/// The payload of a reject. See "Reject form" in `docs/makcu-protocol.md`.
pub(super) const REJECT: u8 = 0xFF;

/// A request to MAKCU. See "Opcodes" in `docs/makcu-protocol.md`.
/// A `SET` that MAKCU accepts has no reply, except `Interpolate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Request {
    Device,
    FirmwareVersion,
    Move { x: i16, y: i16 },
    Interpolate(u8),
    SetBaud(u32),
}

impl Request {
    /// The command byte of the packet. See "Opcodes" in `docs/makcu-protocol.md`.
    pub(super) fn command(self) -> u8 {
        match self {
            Self::Device => CMD_DEVICE,
            Self::FirmwareVersion => CMD_FIRMWARE_VERSION,
            Self::Move { .. } => CMD_MOVE,
            Self::Interpolate(_) => CMD_INTERPOLATE,
            Self::SetBaud(_) => CMD_SET_BAUD,
        }
    }

    /// Appends the packet of the request to `out`.
    pub(super) fn encode(self, out: &mut Vec<u8>) {
        let len = match self {
            Self::Device | Self::FirmwareVersion => 0,
            Self::Move { .. } => 4,
            Self::Interpolate(_) => 1,
            // This one packet has its own length rule. See "Baud switch" in `docs/makcu-protocol.md`.
            Self::SetBaud(_) => 5,
        };
        out.extend_from_slice(&HEADER);
        out.extend_from_slice(&u16::to_le_bytes(len));
        out.push(self.command());
        match self {
            Self::Device | Self::FirmwareVersion => {}
            Self::Move { x, y } => {
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
            }
            Self::Interpolate(value) => out.push(value),
            Self::SetBaud(baud) => out.extend_from_slice(&baud.to_le_bytes()),
        }
    }
}

/// One packet from MAKCU. See "Packet" in `docs/makcu-protocol.md`.
/// The payload is kept in the packet, so a packet is `Copy` and needs no heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Packet {
    pub cmd: u8,
    len: u8,
    bytes: [u8; MAX_PAYLOAD],
}

impl Packet {
    /// `payload` is at most `MAX_PAYLOAD` bytes long. The decoder checks that before it makes a packet.
    fn new(cmd: u8, payload: &[u8]) -> Self {
        let mut bytes = [0; MAX_PAYLOAD];
        bytes[..payload.len()].copy_from_slice(payload);
        Self {
            cmd,
            len: payload.len() as u8,
            bytes,
        }
    }

    pub(super) fn payload(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// A reply that is a reject: the command of the request and the payload `FF`.
/// See "Reject form" in `docs/makcu-protocol.md`.
/// `INTERPOLATE 255` is the exception: its accepted echo has the same bytes, so it is never a reject.
pub(super) fn is_reject(request: Request, packet: &Packet) -> bool {
    packet.payload() == [REJECT] && request != Request::Interpolate(INTERPOLATE_AUTO)
}

/// Whether the reply to `INTERPOLATE value` confirms the value. Auto (255) is never checked,
/// because a reject of Auto has the same bytes as its echo.
pub(super) fn is_interpolate_echo(value: u8, packet: &Packet) -> bool {
    value == INTERPOLATE_AUTO || packet.payload() == [value]
}

/// Whether a reply to `INTERPOLATE value` confirms the value: the echo, and not a reject.
pub(super) fn confirms_interpolate(value: u8, packet: &Packet) -> bool {
    !is_reject(Request::Interpolate(value), packet) && is_interpolate_echo(value, packet)
}

/// Splits a byte stream into packets. Keeps the partial packet between calls.
#[derive(Debug, Default)]
pub(super) struct PacketDecoder {
    buffer: Vec<u8>,
}

impl PacketDecoder {
    /// Calls `on_packet` for each packet that is complete.
    pub(super) fn feed(&mut self, bytes: &[u8], mut on_packet: impl FnMut(Packet)) {
        self.buffer.extend_from_slice(bytes);
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
            on_packet(Packet::new(rest[4], &rest[5..total]));
            start += total;
        }
        self.buffer.drain(..start);
    }
}

/// Whether the mouse route is up, from the `DEVICE` reply. See "Opcodes" in `docs/makcu-protocol.md`.
pub(super) fn mouse_route(kinds: u8) -> bool {
    kinds & KIND_MOUSE != 0
}

/// See "Firmware version" in `docs/makcu-protocol.md`.
pub(super) fn format_firmware(version: u32) -> String {
    format!("V{}.{:03}", version / 1000, version % 1000)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// The bytes that `request` puts on the wire.
    pub(in crate::output) fn frame(request: Request) -> Vec<u8> {
        let mut out = Vec::new();
        request.encode(&mut out);
        out
    }

    fn packet(cmd: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![HEADER[0], HEADER[1], payload.len() as u8, 0, cmd];
        bytes.extend_from_slice(payload);
        bytes
    }

    /// The packets of `bytes`, fed in one call.
    fn decode(bytes: &[u8]) -> Vec<Packet> {
        let mut packets = Vec::new();
        PacketDecoder::default().feed(bytes, |packet| packets.push(packet));
        packets
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
                Request::SetBaud(4_000_000),
                vec![0xDE, 0xAD, 5, 0, 0xA5, 0x00, 0x09, 0x3D, 0x00],
            ),
            (
                Request::SetBaud(115_200),
                vec![0xDE, 0xAD, 5, 0, 0xA5, 0x00, 0xC2, 0x01, 0x00],
            ),
        ];
        for (request, bytes) in cases {
            assert_eq!(frame(request), bytes, "{request:?}");
        }
    }

    #[test]
    fn decoder_returns_joined_packets() {
        let mut bytes = packet(CMD_FIRMWARE_VERSION, &[0xEE, 0x0F, 0, 0]);
        bytes.extend(packet(CMD_INTERPOLATE, &[0xFF]));
        assert_eq!(
            decode(&bytes),
            [
                Packet::new(CMD_FIRMWARE_VERSION, &[0xEE, 0x0F, 0, 0]),
                Packet::new(CMD_INTERPOLATE, &[0xFF]),
            ]
        );
    }

    #[test]
    fn decoder_reads_a_packet_split_at_every_byte() {
        let bytes = packet(CMD_FIRMWARE_VERSION, &[0xEE, 0x0F, 0, 0]);
        let mut decoder = PacketDecoder::default();
        let mut packets = Vec::new();
        for byte in &bytes {
            decoder.feed(&[*byte], |packet| packets.push(packet));
        }
        assert_eq!(
            packets,
            [Packet::new(CMD_FIRMWARE_VERSION, &[0xEE, 0x0F, 0, 0])]
        );
    }

    #[test]
    fn decoder_skips_garbage_before_the_header() {
        let mut bytes = vec![0x00, 0xDE, 0x13, 0xAD];
        bytes.extend(packet(CMD_DEVICE, &[1]));
        assert_eq!(decode(&bytes), [Packet::new(CMD_DEVICE, &[1])]);
    }

    #[test]
    fn decoder_keeps_a_partial_packet_between_calls() {
        let bytes = packet(CMD_DEVICE, &[1]);
        let mut decoder = PacketDecoder::default();
        let mut count = 0;
        decoder.feed(&bytes[..3], |_| count += 1);
        assert_eq!(count, 0);
        decoder.feed(&bytes[3..], |_| count += 1);
        assert_eq!(count, 1);
        decoder.feed(&[], |_| count += 1);
        assert_eq!(count, 1);
    }

    #[test]
    fn decoder_drops_a_packet_with_an_impossible_length() {
        let mut bytes = vec![0xDE, 0xAD, 0xFF, 0xFF, CMD_DEVICE];
        bytes.extend(packet(CMD_FIRMWARE_VERSION, &[1, 2, 3, 4]));
        assert_eq!(
            decode(&bytes),
            [Packet::new(CMD_FIRMWARE_VERSION, &[1, 2, 3, 4])]
        );
    }

    #[test]
    fn decoder_reads_a_reject_packet() {
        assert_eq!(
            decode(&packet(CMD_FIRMWARE_VERSION, &[0xFF])),
            [Packet::new(CMD_FIRMWARE_VERSION, &[0xFF])]
        );
    }

    #[test]
    fn a_reject_is_ff_except_for_the_auto_echo() {
        let reject = Packet::new(CMD_INTERPOLATE, &[0xFF]);
        assert!(is_reject(Request::Interpolate(40), &reject));
        assert!(!is_reject(Request::Interpolate(255), &reject));
        assert!(is_interpolate_echo(255, &reject));
        assert!(!is_interpolate_echo(40, &reject));
    }

    #[test]
    fn firmware_is_formatted_with_three_build_digits() {
        assert_eq!(format_firmware(4085), "V4.085");
        assert_eq!(format_firmware(4094), "V4.094");
    }

    #[test]
    fn only_bit_zero_is_the_mouse_route() {
        assert!(mouse_route(0x01));
        assert!(mouse_route(0x03));
        assert!(!mouse_route(0x02));
        assert!(!mouse_route(0));
    }
}
