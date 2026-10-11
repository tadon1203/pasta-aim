//! The Windows implementation of `Port` and `Connector`. The only `unsafe` in `src/makcu`.
//!
//! The port is an overlapped COM handle: 8N1, no flow control, and 50 ms read and write timeouts.

use std::{io, mem, ptr, time::Duration};

use windows_sys::Win32::Devices::Communication::{
    COMMTIMEOUTS, DCB, GetCommState, NOPARITY, ONESTOPBIT, PURGE_RXABORT, PURGE_RXCLEAR, PurgeComm,
    SetCommState, SetCommTimeouts,
};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DICS_FLAG_GLOBAL, DIGCF_PRESENT, DIREG_DEV, GUID_DEVCLASS_PORTS, HDEVINFO, SP_DEVINFO_DATA,
    SPDRP_HARDWAREID, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceRegistryPropertyW, SetupDiOpenDevRegKey,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_IO_PENDING, GENERIC_READ,
    GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Registry::{HKEY, KEY_READ, REG_SZ, RegCloseKey, RegQueryValueExW};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, ResetEvent, WaitForSingleObject,
};

use super::{Connector, Port, makcu_ports};

const READ_WATCHDOG: Duration = Duration::from_millis(100);
const WRITE_WATCHDOG: Duration = Duration::from_millis(50);
const READ_TIMEOUT_MS: u32 = 50;
const WRITE_TIMEOUT_MS: u32 = 50;

/// The ports of the Windows system. Its `open` gives the port with an empty input buffer.
pub(in crate::makcu) struct WindowsConnector;

impl Connector for WindowsConnector {
    fn ports(&self) -> io::Result<Vec<String>> {
        let devices = port_devices()?;
        let refs: Vec<(&str, &str)> = devices
            .iter()
            .map(|(hardware_id, port)| (hardware_id.as_str(), port.as_str()))
            .collect();
        Ok(makcu_ports(&refs))
    }

    fn open(&self, port: &str, baud: u32) -> io::Result<Box<dyn Port>> {
        Ok(Box::new(WindowsPort::open(port, baud)?))
    }
}

/// The hardware id and the port name of each device of the ports class.
fn port_devices() -> io::Result<Vec<(String, String)>> {
    let set = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVCLASS_PORTS,
            ptr::null(),
            ptr::null_mut(),
            DIGCF_PRESENT,
        )
    };
    if set == -1 {
        return Err(io::Error::last_os_error());
    }
    let mut devices = Vec::new();
    for index in 0u32.. {
        let mut info: SP_DEVINFO_DATA = unsafe { mem::zeroed() };
        info.cbSize = mem::size_of::<SP_DEVINFO_DATA>() as u32;
        if unsafe { SetupDiEnumDeviceInfo(set, index, &mut info) } == 0 {
            break;
        }
        if let (Some(hardware_id), Some(port)) = (hardware_id(set, &info), port_name(set, &info)) {
            devices.push((hardware_id, port));
        }
    }
    unsafe { SetupDiDestroyDeviceInfoList(set) };
    Ok(devices)
}

fn hardware_id(set: HDEVINFO, info: &SP_DEVINFO_DATA) -> Option<String> {
    let mut buffer = [0u16; 512];
    let mut kind = 0;
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set,
            info,
            SPDRP_HARDWAREID,
            &mut kind,
            buffer.as_mut_ptr().cast(),
            mem::size_of_val(&buffer) as u32,
            ptr::null_mut(),
        )
    };
    (ok != 0).then(|| wide_to_string(&buffer))
}

fn port_name(set: HDEVINFO, info: &SP_DEVINFO_DATA) -> Option<String> {
    let key: HKEY =
        unsafe { SetupDiOpenDevRegKey(set, info, DICS_FLAG_GLOBAL, 0, DIREG_DEV, KEY_READ) };
    if key == INVALID_HANDLE_VALUE {
        return None;
    }
    let name: Vec<u16> = "PortName".encode_utf16().chain([0]).collect();
    let mut buffer = [0u16; 256];
    let mut len = mem::size_of_val(&buffer) as u32;
    let mut kind = 0;
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            ptr::null(),
            &mut kind,
            buffer.as_mut_ptr().cast(),
            &mut len,
        )
    };
    unsafe { RegCloseKey(key) };
    (status == 0 && kind == REG_SZ).then(|| wide_to_string(&buffer))
}

/// The text up to the first NUL. A `REG_MULTI_SZ` gives its first string.
fn wide_to_string(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

pub(super) struct WindowsPort {
    handle: HANDLE,
    event: HANDLE,
}

// The handle is used by one reader and one writer. Each has its own handle and event.
unsafe impl Send for WindowsPort {}

impl WindowsPort {
    fn open(name: &str, baud: u32) -> io::Result<Self> {
        let path: Vec<u16> = format!(r"\\.\{name}").encode_utf16().chain([0]).collect();
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let event = match create_event() {
            Ok(event) => event,
            Err(error) => {
                unsafe { CloseHandle(handle) };
                return Err(error);
            }
        };
        let port = Self { handle, event };
        port.configure(baud)?;
        check(unsafe { PurgeComm(port.handle, PURGE_RXABORT | PURGE_RXCLEAR) })?;
        Ok(port)
    }

    fn configure(&self, baud: u32) -> io::Result<()> {
        // Binary mode, and no flag for parity or flow control: the bits are cleared.
        const F_BINARY: u32 = 1 << 0;
        let mut dcb: DCB = unsafe { mem::zeroed() };
        dcb.DCBlength = mem::size_of::<DCB>() as u32;
        check(unsafe { GetCommState(self.handle, &mut dcb) })?;
        dcb.BaudRate = baud;
        dcb.ByteSize = 8;
        dcb.Parity = NOPARITY;
        dcb.StopBits = ONESTOPBIT;
        dcb._bitfield = F_BINARY;
        check(unsafe { SetCommState(self.handle, &dcb) })?;
        let timeouts = COMMTIMEOUTS {
            ReadIntervalTimeout: u32::MAX,
            ReadTotalTimeoutMultiplier: u32::MAX,
            ReadTotalTimeoutConstant: READ_TIMEOUT_MS,
            WriteTotalTimeoutMultiplier: 0,
            WriteTotalTimeoutConstant: WRITE_TIMEOUT_MS,
        };
        check(unsafe { SetCommTimeouts(self.handle, &timeouts) })
    }

    /// One overlapped read or write. It waits at most the watchdog, then cancels.
    fn transfer(&mut self, buffer: *mut u8, len: u32, write: bool) -> io::Result<usize> {
        let watchdog = if write { WRITE_WATCHDOG } else { READ_WATCHDOG };
        let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
        overlapped.hEvent = self.event;
        let mut transferred = 0u32;
        check(unsafe { ResetEvent(self.event) })?;
        let started = unsafe {
            if write {
                WriteFile(self.handle, buffer, len, ptr::null_mut(), &mut overlapped)
            } else {
                ReadFile(self.handle, buffer, len, ptr::null_mut(), &mut overlapped)
            }
        };
        if started == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error);
            }
            match unsafe { WaitForSingleObject(self.event, watchdog.as_millis() as u32) } {
                WAIT_OBJECT_0 => {}
                WAIT_TIMEOUT => {
                    unsafe { CancelIoEx(self.handle, &overlapped) };
                    // The transfer may have finished before the cancel. Then its count is the answer.
                    let completed = unsafe {
                        GetOverlappedResult(self.handle, &overlapped, &mut transferred, 1)
                    };
                    return if completed != 0 {
                        Ok(transferred as usize)
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "COM transfer timed out",
                        ))
                    };
                }
                _ => {
                    let error = io::Error::last_os_error();
                    unsafe { CancelIoEx(self.handle, &overlapped) };
                    return Err(error);
                }
            }
        }
        check(unsafe { GetOverlappedResult(self.handle, &overlapped, &mut transferred, 1) })?;
        Ok(transferred as usize)
    }
}

impl Port for WindowsPort {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.transfer(buf.as_mut_ptr(), buf.len() as u32, false)? {
            0 => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no reply from MAKCU",
            )),
            count => Ok(count),
        }
    }

    fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let count = self.transfer(bytes.as_ptr() as *mut u8, bytes.len() as u32, true)?;
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

    fn try_clone(&self) -> io::Result<Box<dyn Port>> {
        let mut duplicate: HANDLE = ptr::null_mut();
        check(unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.handle,
                GetCurrentProcess(),
                &mut duplicate,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        })?;
        match create_event() {
            Ok(event) => Ok(Box::new(Self {
                handle: duplicate,
                event,
            })),
            Err(error) => {
                unsafe { CloseHandle(duplicate) };
                Err(error)
            }
        }
    }
}

impl Drop for WindowsPort {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.event);
            CloseHandle(self.handle);
        }
    }
}

fn create_event() -> io::Result<HANDLE> {
    let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if event.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(event)
    }
}

/// Turns a Windows `BOOL` into an `io::Result`.
fn check(ok: i32) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
