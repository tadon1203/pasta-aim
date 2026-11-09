use std::{
    io,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail, ensure};

use super::{
    Device, Port as Entry,
    port::{Connector, Serial},
    protocol::{Packet, PacketDecoder, Query, is_reject, mouse_route},
};

pub(super) const LOW_BAUD: u32 = 115_200;
pub(super) const HIGH_BAUD: u32 = 4_000_000;
const REPLY_TIMEOUT: Duration = Duration::from_millis(100);

pub(super) fn connect(
    connector: &dyn Connector,
    chosen: Option<&str>,
) -> Result<(Box<dyn Serial>, Device)> {
    let names = match chosen {
        Some(name) => vec![name.to_owned()],
        None => makcu_ports(connector.ports()?),
    };
    let mut failures = Vec::new();
    for name in &names {
        match connect_port(connector, name) {
            Ok(connected) => return Ok(connected),
            Err(error) => failures.push(format!("{name}: {error:#}")),
        }
    }
    if failures.is_empty() {
        bail!("no MAKCU port found");
    }
    bail!("{}", failures.join("; "))
}

fn makcu_ports(entries: Vec<Entry>) -> Vec<String> {
    entries
        .into_iter()
        .filter(|entry| !entry.description.is_empty())
        .map(|entry| entry.name)
        .collect()
}

fn connect_port(connector: &dyn Connector, name: &str) -> Result<(Box<dyn Serial>, Device)> {
    // The rate that the device keeps is unknown after the app closes, so both rates are probed.
    let (mut port, mut baud) = probe_first(connector, name, [LOW_BAUD, HIGH_BAUD])?;
    let firmware = firmware_of(port.as_mut())?;
    if baud == LOW_BAUD {
        // A `SetBaud` that the device accepts has no reply.
        write_queries(port.as_mut(), &mut Vec::new(), &[Query::SetBaud(HIGH_BAUD)])?;
        drop(port);
        (port, baud) = probe_first(connector, name, [HIGH_BAUD, LOW_BAUD])?;
    }
    let kinds = kinds_of(port.as_mut())?;
    let device = Device {
        port: name.to_owned(),
        firmware,
        baud,
        mouse_route: mouse_route(kinds),
    };
    Ok((port, device))
}

fn probe_first(
    connector: &dyn Connector,
    name: &str,
    rates: [u32; 2],
) -> Result<(Box<dyn Serial>, u32)> {
    let mut result = Err(anyhow!("MAKCU did not answer at either baud rate"));
    for baud in rates {
        result = probe(connector, name, baud).map(|port| (port, baud));
        if result.is_ok() {
            break;
        }
    }
    result
}

fn probe(connector: &dyn Connector, name: &str, baud: u32) -> Result<Box<dyn Serial>> {
    let mut port = connector.open(name, baud)?;
    exchange_accepted(port.as_mut(), Query::Device)?;
    Ok(port)
}

fn firmware_of(port: &mut dyn Serial) -> Result<u32> {
    let packet = exchange_accepted(port, Query::FirmwareVersion)?;
    let [a, b, c, d] = *packet.payload() else {
        bail!("MAKCU sent a malformed firmware version");
    };
    Ok(u32::from_le_bytes([a, b, c, d]))
}

fn kinds_of(port: &mut dyn Serial) -> Result<u8> {
    let packet = exchange_accepted(port, Query::Device)?;
    let [kinds] = *packet.payload() else {
        bail!("MAKCU sent a malformed device reply");
    };
    Ok(kinds)
}

fn exchange_accepted(port: &mut dyn Serial, query: Query) -> Result<Packet> {
    let packet = exchange(port, query)?;
    ensure!(!is_reject(query, &packet), "MAKCU rejected {query:?}");
    Ok(packet)
}

/// A reject is a reply too, so the caller checks it. The port must have no other reader.
pub(super) fn exchange(port: &mut dyn Serial, query: Query) -> Result<Packet> {
    write_queries(port, &mut Vec::new(), std::slice::from_ref(&query))?;
    let mut decoder = PacketDecoder::default();
    let mut answer = None;
    let deadline = Instant::now() + REPLY_TIMEOUT;
    while answer.is_none() && Instant::now() < deadline {
        read_packets(port, &mut decoder, |packet| {
            if answer.is_none() && packet.cmd == query.command() {
                answer = Some(packet);
            }
        })?;
    }
    answer.ok_or_else(|| anyhow!("MAKCU did not answer {query:?} in time"))
}

fn read_packets(
    port: &mut dyn Serial,
    decoder: &mut PacketDecoder,
    on_packet: impl FnMut(Packet),
) -> io::Result<()> {
    let mut buf = [0u8; 256];
    match port.read(&mut buf) {
        Ok(count) => {
            decoder.feed(&buf[..count], on_packet);
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::TimedOut => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn write_queries(
    port: &mut dyn Serial,
    buffer: &mut Vec<u8>,
    queries: &[Query],
) -> io::Result<()> {
    buffer.clear();
    for query in queries {
        query.encode(buffer);
    }
    port.write_all(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{
        fake::FakeMakcu,
        protocol::{CMD_INTERPOLATE, CMD_SET_BAUD, tests::frame},
    };

    fn entry(name: &str, description: &str) -> Entry {
        Entry {
            name: name.to_owned(),
            description: description.to_owned(),
        }
    }

    #[test]
    fn connect_switches_the_device_to_4_000_000_baud() {
        let fake = FakeMakcu::new();
        let (_port, device) = connect(&fake, None).unwrap();
        assert_eq!(device.baud, HIGH_BAUD);
        assert!(fake.frames().contains(&frame(Query::SetBaud(HIGH_BAUD))));
    }

    #[test]
    fn connect_keeps_115_200_baud_when_the_device_refuses_the_switch() {
        let fake = FakeMakcu::new();
        fake.set_accepted_bauds(&[LOW_BAUD]);
        let (_port, device) = connect(&fake, None).unwrap();
        assert_eq!(device.baud, LOW_BAUD);
        assert!(fake.frames().contains(&frame(Query::SetBaud(HIGH_BAUD))));
    }

    #[test]
    fn connect_finds_a_device_already_at_4_000_000_baud_and_skips_the_switch() {
        let fake = FakeMakcu::new();
        fake.set_baud(HIGH_BAUD);
        fake.set_accepted_bauds(&[HIGH_BAUD]);
        let (_port, device) = connect(&fake, None).unwrap();
        assert_eq!(device.baud, HIGH_BAUD);
        assert!(!fake.frames().iter().any(|frame| frame[4] == CMD_SET_BAUD));
    }

    #[test]
    fn the_firmware_is_read_and_no_version_is_refused() {
        for firmware in [4000, 4075, 4094] {
            let fake = FakeMakcu::new();
            fake.set_firmware(firmware);
            let (_port, device) = connect(&fake, None).unwrap();
            assert_eq!(device.firmware, firmware);
        }
    }

    #[test]
    fn the_mouse_route_follows_the_device_bit() {
        let fake = FakeMakcu::new();
        fake.set_kinds(0x03);
        let (_port, device) = connect(&fake, None).unwrap();
        assert!(device.mouse_route);

        fake.set_kinds(0x02);
        let (_port, device) = connect(&fake, None).unwrap();
        assert!(!device.mouse_route);
    }

    #[test]
    fn connect_sends_no_interpolation() {
        let fake = FakeMakcu::new();
        connect(&fake, None).unwrap();
        assert!(
            !fake
                .frames()
                .iter()
                .any(|frame| frame[4] == CMD_INTERPOLATE)
        );
    }

    #[test]
    fn only_ports_with_a_makcu_chip_are_tried_when_none_is_chosen() {
        let entries = vec![
            entry("COM4", "MAKCU"),
            entry("COM3", "MAKCU"),
            entry("COM2", ""),
        ];
        assert_eq!(makcu_ports(entries), ["COM4", "COM3"]);
        assert_eq!(makcu_ports(vec![entry("COM2", "")]), Vec::<String>::new());
    }

    #[test]
    fn a_chosen_port_is_the_only_one_tried_and_its_failure_names_it() {
        let fake = FakeMakcu::new();
        let error = connect(&fake, Some("COM9")).err().unwrap();
        assert!(format!("{error:#}").starts_with("COM9: "), "{error:#}");
    }

    #[test]
    fn a_reject_reply_is_returned_by_exchange() {
        let fake = FakeMakcu::new();
        let (mut port, _) = connect(&fake, None).unwrap();
        fake.reject_next(CMD_INTERPOLATE);
        let reply = exchange(port.as_mut(), Query::Interpolate(40)).unwrap();
        assert_eq!(reply.payload(), [0xFF]);
    }

    #[test]
    fn the_auto_echo_of_interpolate_255_is_an_answer_not_a_reject() {
        let fake = FakeMakcu::new();
        let (mut port, _) = connect(&fake, None).unwrap();
        let reply = exchange_accepted(port.as_mut(), Query::Interpolate(255)).unwrap();
        assert_eq!(reply.payload(), [0xFF]);
    }

    #[test]
    fn a_request_without_a_reply_times_out() {
        let fake = FakeMakcu::new();
        let (mut port, _) = connect(&fake, None).unwrap();
        fake.mute_next(CMD_INTERPOLATE);
        let error = exchange(port.as_mut(), Query::Interpolate(40)).unwrap_err();
        assert!(format!("{error:#}").contains("in time"), "{error:#}");
    }
}
