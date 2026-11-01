//! The serial port of MAKCU: finding the ports and opening them.
//!
//! `Serial` and `Connector` are the seam. The Windows implementation is below, and `fake` (test only)
//! is a MAKCU in memory. The Windows code is the only `unsafe` in `src/output`.

use std::{io, mem, time::Duration};

use windows::{
    Win32::{
        Devices::{
            Communication::{
                COMMTIMEOUTS, ClearCommError, DCB, GetCommState, NOPARITY, ONESTOPBIT,
                PURGE_COMM_FLAGS, PURGE_RXABORT, PURGE_RXCLEAR, PurgeComm, SetCommState,
                SetCommTimeouts,
            },
            DeviceAndDriverInstallation::{
                DICS_FLAG_GLOBAL, DIGCF_PRESENT, DIREG_DEV, GUID_DEVCLASS_PORTS, HDEVINFO,
                SP_DEVINFO_DATA, SPDRP_HARDWAREID, SetupDiDestroyDeviceInfoList,
                SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW,
                SetupDiOpenDevRegKey,
            },
        },
        Foundation::{
            CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE, HWND,
            WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES,
            FILE_SHARE_MODE, OPEN_EXISTING, ReadFile, WriteFile,
        },
        System::{
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
            Registry::{HKEY, KEY_READ, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegQueryValueExW},
            Threading::{CreateEventW, WaitForSingleObject},
        },
    },
    core::PCWSTR,
};

use super::Port as Entry;

/// A port that is open. `read` gives `TimedOut` when nothing came.
pub(super) trait Serial: Send {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// Fails when the device is gone. It does not wait.
    fn check(&mut self) -> io::Result<()>;
}

pub(super) trait Connector: Send + Sync {
    /// Every serial port of the system, the MAKCU chips first. See `list_ports`.
    fn ports(&self) -> io::Result<Vec<Entry>>;
    /// Opens the port at `baud`. The input buffer is empty when it returns.
    fn open(&self, port: &str, baud: u32) -> io::Result<Box<dyn Serial>>;
}

/// The USB ids of the chips of MAKCU: CH343 first, then CH340.
const CHIPS: [(u16, u16); 2] = [(0x1A86, 0x55D3), (0x1A86, 0x7523)];

/// Every port, the MAKCU chips first (CH343 before CH340), then the others by name.
/// Each device is its hardware id and its port name.
pub(super) fn list_ports(devices: &[(&str, &str)]) -> Vec<Entry> {
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
        .map(|(rank, port)| Entry {
            name: port.to_owned(),
            description: if rank.is_some() {
                "MAKCU".to_owned()
            } else {
                String::new()
            },
        })
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

/// The ports of the Windows system. Its `open` gives the port with an empty input buffer.
pub(in crate::output) struct WindowsConnector;

impl Connector for WindowsConnector {
    fn ports(&self) -> io::Result<Vec<Entry>> {
        Ok(list_ports(&as_refs(&port_devices()?)))
    }

    fn open(&self, port: &str, baud: u32) -> io::Result<Box<dyn Serial>> {
        Ok(Box::new(WindowsPort::open(port, baud)?))
    }
}

fn as_refs(devices: &[(String, String)]) -> Vec<(&str, &str)> {
    devices
        .iter()
        .map(|(hardware_id, port)| (hardware_id.as_str(), port.as_str()))
        .collect()
}

/// The hardware id and the port name of each device of the ports class.
fn port_devices() -> io::Result<Vec<(String, String)>> {
    let set = unsafe {
        SetupDiGetClassDevsW(
            Some(std::ptr::from_ref(&GUID_DEVCLASS_PORTS)),
            PCWSTR::null(),
            HWND::default(),
            DIGCF_PRESENT,
        )
    }
    .map_err(|error| os_error(&error))?;
    let mut devices = Vec::new();
    for index in 0u32.. {
        let mut info: SP_DEVINFO_DATA = unsafe { mem::zeroed() };
        info.cbSize = mem::size_of::<SP_DEVINFO_DATA>() as u32;
        if unsafe { SetupDiEnumDeviceInfo(set, index, &mut info) }.is_err() {
            break;
        }
        if let (Some(hardware_id), Some(port)) = (hardware_id(set, &info), port_name(set, &info)) {
            devices.push((hardware_id, port));
        }
    }
    let _ = unsafe { SetupDiDestroyDeviceInfoList(set) };
    Ok(devices)
}

fn hardware_id(set: HDEVINFO, info: &SP_DEVINFO_DATA) -> Option<String> {
    let mut buffer = [0u16; 512];
    // SAFETY: the buffer is `buffer.len()` u16 values, so its bytes are that many times two, and it is
    // only read after the call returns.
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), mem::size_of_val(&buffer))
    };
    unsafe {
        SetupDiGetDeviceRegistryPropertyW(set, info, SPDRP_HARDWAREID, None, Some(bytes), None)
    }
    .ok()?;
    Some(wide_to_string(&buffer))
}

fn port_name(set: HDEVINFO, info: &SP_DEVINFO_DATA) -> Option<String> {
    let key: HKEY =
        unsafe { SetupDiOpenDevRegKey(set, info, DICS_FLAG_GLOBAL.0, 0, DIREG_DEV, KEY_READ.0) }
            .ok()?;
    let name: Vec<u16> = "PortName".encode_utf16().chain([0]).collect();
    let mut buffer = [0u16; 256];
    let mut len = mem::size_of_val(&buffer) as u32;
    let mut kind = REG_VALUE_TYPE(0);
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    let _ = unsafe { RegCloseKey(key) };
    (status.is_ok() && kind == REG_SZ).then(|| wide_to_string(&buffer))
}

/// The text up to the first NUL. A `REG_MULTI_SZ` gives its first string.
fn wide_to_string(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// Converts a Windows error to the `io::Error` of its Win32 code.
fn os_error(error: &windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(error.code().0 & 0xFFFF)
}

/// The read gave no bytes in time. It is the one `TimedOut` that the port reports.
fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "no reply from MAKCU")
}

const READ_WATCHDOG: Duration = Duration::from_millis(100);
const WRITE_WATCHDOG: Duration = Duration::from_millis(50);
const READ_TIMEOUT_MS: u32 = 50;
const WRITE_TIMEOUT_MS: u32 = 50;

pub(super) struct WindowsPort {
    handle: HANDLE,
    event: HANDLE,
}

// The port is used by one thread at a time: the engine thread.
unsafe impl Send for WindowsPort {}

impl WindowsPort {
    fn open(name: &str, baud: u32) -> io::Result<Self> {
        let path: Vec<u16> = format!(r"\\.\{name}").encode_utf16().chain([0]).collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(FILE_ATTRIBUTE_NORMAL.0 | FILE_FLAG_OVERLAPPED.0),
                HANDLE::default(),
            )
        }
        .map_err(|error| os_error(&error))?;
        let event = match create_event() {
            Ok(event) => event,
            Err(error) => {
                let _ = unsafe { CloseHandle(handle) };
                return Err(error);
            }
        };
        let port = Self { handle, event };
        port.configure(baud)?;
        unsafe {
            PurgeComm(
                port.handle,
                PURGE_COMM_FLAGS(PURGE_RXABORT.0 | PURGE_RXCLEAR.0),
            )
        }
        .map_err(|error| os_error(&error))?;
        Ok(port)
    }

    fn configure(&self, baud: u32) -> io::Result<()> {
        // Binary mode, and no flag for parity or flow control: the bits are cleared.
        const F_BINARY: u32 = 1 << 0;
        let mut dcb: DCB = unsafe { mem::zeroed() };
        dcb.DCBlength = mem::size_of::<DCB>() as u32;
        unsafe { GetCommState(self.handle, &mut dcb) }.map_err(|error| os_error(&error))?;
        dcb.BaudRate = baud;
        dcb.ByteSize = 8;
        dcb.Parity = NOPARITY;
        dcb.StopBits = ONESTOPBIT;
        dcb._bitfield = F_BINARY;
        unsafe { SetCommState(self.handle, &dcb) }.map_err(|error| os_error(&error))?;
        let timeouts = COMMTIMEOUTS {
            ReadIntervalTimeout: u32::MAX,
            ReadTotalTimeoutMultiplier: u32::MAX,
            ReadTotalTimeoutConstant: READ_TIMEOUT_MS,
            WriteTotalTimeoutMultiplier: 0,
            WriteTotalTimeoutConstant: WRITE_TIMEOUT_MS,
        };
        unsafe { SetCommTimeouts(self.handle, &timeouts) }.map_err(|error| os_error(&error))
    }

    /// Runs one overlapped transfer that `start` begins. It waits at most `watchdog`, then cancels,
    /// and returns the number of bytes that moved.
    fn transfer(
        &self,
        watchdog: Duration,
        start: impl FnOnce(&mut OVERLAPPED) -> windows::core::Result<()>,
    ) -> io::Result<usize> {
        let mut overlapped = OVERLAPPED {
            hEvent: self.event,
            ..OVERLAPPED::default()
        };
        match start(&mut overlapped) {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_IO_PENDING.to_hresult() => {
                let watchdog_ms = watchdog.as_millis() as u32;
                match unsafe { WaitForSingleObject(self.event, watchdog_ms) } {
                    WAIT_OBJECT_0 => {}
                    WAIT_TIMEOUT => {
                        let _ = unsafe { CancelIoEx(self.handle, Some(&overlapped)) };
                        // The transfer may have finished before the cancel. Then its count is the answer.
                        let mut transferred = 0u32;
                        return match unsafe {
                            GetOverlappedResult(self.handle, &overlapped, &mut transferred, true)
                        } {
                            Ok(()) => Ok(transferred as usize),
                            Err(_) => Err(timed_out()),
                        };
                    }
                    _ => {
                        let error = io::Error::last_os_error();
                        let _ = unsafe { CancelIoEx(self.handle, Some(&overlapped)) };
                        return Err(error);
                    }
                }
            }
            Err(error) => return Err(os_error(&error)),
        }
        let mut transferred = 0u32;
        unsafe { GetOverlappedResult(self.handle, &overlapped, &mut transferred, true) }
            .map_err(|error| os_error(&error))?;
        Ok(transferred as usize)
    }
}

impl Serial for WindowsPort {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = self.transfer(READ_WATCHDOG, |overlapped| unsafe {
            ReadFile(
                self.handle,
                Some(buf),
                None,
                Some(std::ptr::from_mut(overlapped)),
            )
        })?;
        match count {
            0 => Err(timed_out()),
            count => Ok(count),
        }
    }

    fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let count = self.transfer(WRITE_WATCHDOG, |overlapped| unsafe {
                WriteFile(
                    self.handle,
                    Some(bytes),
                    None,
                    Some(std::ptr::from_mut(overlapped)),
                )
            })?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "COM write wrote zero bytes",
                ));
            }
            bytes = &bytes[count..];
        }
        Ok(())
    }

    /// `ClearCommError` fails once the USB device is removed. It reads no byte and does not wait.
    fn check(&mut self) -> io::Result<()> {
        unsafe { ClearCommError(self.handle, None, None) }.map_err(|error| os_error(&error))
    }
}

impl Drop for WindowsPort {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.event) };
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

fn create_event() -> io::Result<HANDLE> {
    unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(|error| os_error(&error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, description: &str) -> Entry {
        Entry {
            name: name.to_owned(),
            description: description.to_owned(),
        }
    }

    #[test]
    fn every_port_is_listed_with_the_makcu_chips_first_and_the_rest_by_name() {
        let devices = [
            (r"ACPI\PNP0501", "COM10"),
            (r"USB\VID_1A86&PID_7523", "COM3"),
            (r"ACPI\PNP0501", "COM2"),
            (r"USB\VID_1A86&PID_55D3", "COM4"),
        ];
        assert_eq!(
            list_ports(&devices),
            [
                entry("COM4", "MAKCU"),
                entry("COM3", "MAKCU"),
                entry("COM2", ""),
                entry("COM10", ""),
            ]
        );
    }

    #[test]
    fn only_the_exact_makcu_chip_ids_are_labeled_in_any_letter_case() {
        let devices = [
            (r"USB\VID_1A86&PID_7523&MI_00", "COM3"),
            (r"USB\VID_1A87&PID_55D3", "COM5"),
            (r"USB\VID_1A86&PID_7524", "COM6"),
            (r"usb\vid_1a86&pid_7523", "COM7"),
        ];
        assert_eq!(
            list_ports(&devices),
            [
                entry("COM3", "MAKCU"),
                entry("COM7", "MAKCU"),
                entry("COM5", ""),
                entry("COM6", ""),
            ]
        );
    }
}
