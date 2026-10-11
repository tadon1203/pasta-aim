//! The serial port of MAKCU: finding the ports and opening them.
//!
//! `Port` and `Connector` are the seam. `windows` talks to the operating system. `fake` (test only)
//! is a MAKCU in memory.

use std::io;

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

/// The USB ids of the chips of MAKCU: CH343 first, then CH340.
const CHIPS: [(u16, u16); 2] = [(0x1A86, 0x55D3), (0x1A86, 0x7523)];

/// Keeps the ports of the MAKCU chips, CH343 first. Each device is its hardware id and its port name.
pub(super) fn makcu_ports(devices: &[(&str, &str)]) -> Vec<String> {
    let mut ports: Vec<(usize, &str)> = devices
        .iter()
        .filter_map(|&(hardware_id, port)| {
            let ids = usb_ids(hardware_id)?;
            let rank = CHIPS.iter().position(|&chip| chip == ids)?;
            Some((rank, port))
        })
        .collect();
    ports.sort_by_key(|&(rank, _)| rank);
    ports.into_iter().map(|(_, port)| port.to_owned()).collect()
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
    use super::makcu_ports;

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
        assert!(makcu_ports(&[(r"USB\VID_046D&PID_C52B", "COM9")]).is_empty());
        assert!(makcu_ports(&[]).is_empty());
    }
}
