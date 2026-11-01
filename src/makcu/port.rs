//! The serial port of MAKCU: finding the ports and opening them.
//!
//! `Port` and `Connector` are the seam. `windows` talks to the operating system. `fake` (test only)
//! is a MAKCU in memory.

use std::{io, sync::Arc};

#[cfg(test)]
pub(super) mod fake;
pub(super) mod windows;

/// A port that is open. `read` gives `TimedOut` when nothing came.
pub(super) trait Port: Send {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn try_clone(&self) -> io::Result<Box<dyn Port>>;
}

pub(super) trait Connector: Send + Sync {
    fn ports(&self) -> io::Result<Vec<String>>;
    /// Opens the port at `baud`. The input buffer is empty when it returns.
    fn open(&self, port: &str, baud: u32) -> io::Result<Box<dyn Port>>;
}

/// Tries only the port that the user chose, even when its chip is not one of MAKCU.
pub(super) struct ManualConnector {
    inner: Arc<dyn Connector>,
    port: String,
}

impl ManualConnector {
    pub(super) fn new(inner: Arc<dyn Connector>, port: String) -> Self {
        Self { inner, port }
    }
}

impl Connector for ManualConnector {
    fn ports(&self) -> io::Result<Vec<String>> {
        Ok(vec![self.port.clone()])
    }

    fn open(&self, port: &str, baud: u32) -> io::Result<Box<dyn Port>> {
        self.inner.open(port, baud)
    }
}

/// A serial port of the system, as the port selector lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortEntry {
    pub name: String,
    /// The chip of the port is one of MAKCU, so auto-detect tries it.
    pub makcu: bool,
}

/// The USB ids of the chips of MAKCU: CH343 first, then CH340.
const CHIPS: [(u16, u16); 2] = [(0x1A86, 0x55D3), (0x1A86, 0x7523)];

/// Every port, the MAKCU chips first (CH343 before CH340), then the others by name.
/// Each device is its hardware id and its port name.
pub(super) fn list_ports(devices: &[(&str, &str)]) -> Vec<PortEntry> {
    let mut ports: Vec<(Option<usize>, &str)> = devices
        .iter()
        .map(|&(hardware_id, port)| {
            let rank =
                usb_ids(hardware_id).and_then(|ids| CHIPS.iter().position(|&chip| chip == ids));
            (rank, port)
        })
        .collect();
    // `None` sorts first in an `Option`, so the rank is mapped to put it last. `COM2` sorts before `COM10`.
    ports.sort_by_key(|&(rank, port)| (rank.unwrap_or(usize::MAX), port.len(), port));
    ports
        .into_iter()
        .map(|(rank, port)| PortEntry {
            name: port.to_owned(),
            makcu: rank.is_some(),
        })
        .collect()
}

/// Keeps the ports of the MAKCU chips, CH343 first.
pub(super) fn makcu_ports(devices: &[(&str, &str)]) -> Vec<String> {
    list_ports(devices)
        .into_iter()
        .filter(|entry| entry.makcu)
        .map(|entry| entry.name)
        .collect()
}

/// The VID and the PID of a hardware id such as `USB\VID_1A86&PID_55D3&MI_00`.
fn usb_ids(hardware_id: &str) -> Option<(u16, u16)> {
    let upper = hardware_id.to_ascii_uppercase();
    let vid = upper.split("VID_").nth(1)?.get(..4)?;
    let pid = upper.split("PID_").nth(1)?.get(..4)?;
    Some((
        u16::from_str_radix(vid, 16).ok()?,
        u16::from_str_radix(pid, 16).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Connector, ManualConnector, PortEntry, list_ports, makcu_ports};
    use crate::makcu::port::fake::FakeMakcu;

    #[test]
    fn every_port_is_listed_with_the_makcu_chips_first_and_the_rest_by_name() {
        let devices = [
            (r"ACPI\PNP0501", "COM10"),
            (r"USB\VID_1A86&PID_7523", "COM3"),
            (r"ACPI\PNP0501", "COM2"),
            (r"USB\VID_1A86&PID_55D3", "COM4"),
        ];
        let entry = |name: &str, makcu| PortEntry {
            name: name.to_owned(),
            makcu,
        };
        assert_eq!(
            list_ports(&devices),
            [
                entry("COM4", true),
                entry("COM3", true),
                entry("COM2", false),
                entry("COM10", false),
            ]
        );
    }

    #[test]
    fn a_manual_connector_offers_only_the_chosen_port() {
        let manual = ManualConnector::new(Arc::new(FakeMakcu::new()), "COM9".to_owned());
        assert_eq!(manual.ports().unwrap(), ["COM9"]);
    }

    #[test]
    fn only_the_makcu_chips_are_kept_and_ch343_comes_first() {
        let devices = [
            (r"USB\VID_1A86&PID_7523&MI_00", "COM3"),
            (r"USB\VID_1A86&PID_55D3", "COM4"),
            (r"USB\VID_1A87&PID_55D3", "COM5"),
            (r"USB\VID_1A86&PID_7524", "COM6"),
            (r"ACPI\PNP0501", "COM1"),
            (r"usb\vid_1a86&pid_7523", "COM7"),
        ];
        assert_eq!(makcu_ports(&devices), ["COM4", "COM3", "COM7"]);
    }

    #[test]
    fn no_makcu_chip_gives_no_port() {
        assert_eq!(makcu_ports(&[(r"USB\VID_046D&PID_C52B", "COM9")]).len(), 0);
        assert_eq!(makcu_ports(&[]).len(), 0);
    }
}
