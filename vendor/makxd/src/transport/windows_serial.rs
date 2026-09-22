//! Windows overlapped COM carrier. `serialport` is deliberately not used for I/O.
use std::io;
use std::time::{Duration, Instant};

use windows_sys::Win32::Devices::Communication::{
    COMMTIMEOUTS, COMSTAT, ClearCommError, DCB, GetCommState, NOPARITY, ONESTOPBIT, PURGE_RXABORT,
    PURGE_RXCLEAR, PurgeComm, SetCommState, SetCommTimeouts,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_IO_PENDING, GENERIC_READ,
    GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, ResetEvent, WaitForSingleObject,
};

use super::wire::WirePort;
use crate::error::{MakxdError, Result};

const READ_WATCHDOG: Duration = Duration::from_millis(100);
const WRITE_WATCHDOG: Duration = Duration::from_millis(50);
const READ_TIMEOUT_MS: u32 = 50;
const WRITE_TIMEOUT_MS: u32 = 50;

fn normalized_port_name(name: &str) -> String {
    if name.starts_with(r"\\.\") {
        name.to_owned()
    } else {
        format!(r"\\.\{name}")
    }
}

fn create_io_event() -> io::Result<HANDLE> {
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(event)
    }
}

pub(crate) struct WindowsSerialPort {
    handle: HANDLE,
    event: HANDLE,
}
unsafe impl Send for WindowsSerialPort {}

impl WindowsSerialPort {
    pub fn open(name: &str, baud: u32) -> Result<Self> {
        let path: Vec<u16> = normalized_port_name(name)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(MakxdError::Io(io::Error::last_os_error()));
        }
        let event = match create_io_event() {
            Ok(event) => event,
            Err(error) => {
                unsafe { CloseHandle(handle) };
                return Err(MakxdError::Io(error));
            }
        };
        let port = Self { handle, event };
        port.configure(baud)?;
        Ok(port)
    }

    fn configure(&self, baud: u32) -> Result<()> {
        unsafe {
            let mut dcb: DCB = std::mem::zeroed();
            dcb.DCBlength = std::mem::size_of::<DCB>() as u32;
            if GetCommState(self.handle, &mut dcb) == 0 {
                return Err(MakxdError::Io(io::Error::last_os_error()));
            }
            dcb.BaudRate = baud;
            dcb.ByteSize = 8;
            dcb.Parity = NOPARITY;
            dcb.StopBits = ONESTOPBIT;
            const F_BINARY: u32 = 1 << 0;
            const F_PARITY: u32 = 1 << 1;
            const F_OUTX_CTS_FLOW: u32 = 1 << 2;
            const F_OUTX_DSR_FLOW: u32 = 1 << 3;
            const F_DTR_CONTROL: u32 = 0b11 << 4;
            const F_DSR_SENSITIVITY: u32 = 1 << 6;
            const F_TX_CONTINUE_ON_XOFF: u32 = 1 << 7;
            const F_OUT_X: u32 = 1 << 8;
            const F_IN_X: u32 = 1 << 9;
            const F_RTS_CONTROL: u32 = 0b11 << 12;
            const SERIAL_MODE_FLAGS: u32 = F_BINARY
                | F_PARITY
                | F_OUTX_CTS_FLOW
                | F_OUTX_DSR_FLOW
                | F_DTR_CONTROL
                | F_DSR_SENSITIVITY
                | F_TX_CONTINUE_ON_XOFF
                | F_OUT_X
                | F_IN_X
                | F_RTS_CONTROL;
            dcb._bitfield = (dcb._bitfield & !SERIAL_MODE_FLAGS) | F_BINARY;
            if SetCommState(self.handle, &dcb) == 0 {
                return Err(MakxdError::Io(io::Error::last_os_error()));
            }
            let timeouts = COMMTIMEOUTS {
                ReadIntervalTimeout: u32::MAX,
                ReadTotalTimeoutMultiplier: u32::MAX,
                ReadTotalTimeoutConstant: READ_TIMEOUT_MS,
                WriteTotalTimeoutMultiplier: 0,
                WriteTotalTimeoutConstant: WRITE_TIMEOUT_MS,
            };
            if SetCommTimeouts(self.handle, &timeouts) == 0 {
                return Err(MakxdError::Io(io::Error::last_os_error()));
            }
        }
        Ok(())
    }

    pub fn clear_input(&mut self) -> Result<()> {
        unsafe {
            if PurgeComm(self.handle, PURGE_RXABORT | PURGE_RXCLEAR) == 0 {
                Err(MakxdError::Io(io::Error::last_os_error()))
            } else {
                Ok(())
            }
        }
    }

    fn io(&mut self, bytes: *mut u8, len: u32, write: bool) -> io::Result<usize> {
        unsafe {
            if ResetEvent(self.event) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut overlapped: OVERLAPPED = std::mem::zeroed();
            overlapped.hEvent = self.event;
            let ok = if write {
                WriteFile(
                    self.handle,
                    bytes,
                    len,
                    std::ptr::null_mut(),
                    &mut overlapped,
                )
            } else {
                ReadFile(
                    self.handle,
                    bytes,
                    len,
                    std::ptr::null_mut(),
                    &mut overlapped,
                )
            };
            let mut pending = false;
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(error);
                }
                pending = true;
                let watchdog = if write { WRITE_WATCHDOG } else { READ_WATCHDOG };
                match WaitForSingleObject(self.event, watchdog.as_millis() as u32) {
                    WAIT_OBJECT_0 => {}
                    WAIT_TIMEOUT => {
                        let _ = CancelIoEx(self.handle, &overlapped);
                        let mut transferred = 0u32;
                        let completed =
                            GetOverlappedResult(self.handle, &mut overlapped, &mut transferred, 1)
                                != 0;
                        return if completed {
                            Ok(transferred as usize)
                        } else {
                            Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "overlapped COM I/O timed out",
                            ))
                        };
                    }
                    _ => {
                        let error = io::Error::last_os_error();
                        let _ = CancelIoEx(self.handle, &overlapped);
                        let mut transferred = 0u32;
                        let _ =
                            GetOverlappedResult(self.handle, &mut overlapped, &mut transferred, 1);
                        return Err(error);
                    }
                }
            }
            let mut transferred = 0u32;
            if GetOverlappedResult(
                self.handle,
                &mut overlapped,
                &mut transferred,
                pending as i32,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(transferred as usize)
        }
    }
}

impl Drop for WindowsSerialPort {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.event);
            CloseHandle(self.handle);
        }
    }
}

impl WirePort for WindowsSerialPort {
    fn try_clone_wire(&self) -> Result<Box<dyn WirePort>> {
        unsafe {
            let mut duplicate = std::ptr::null_mut();
            if DuplicateHandle(
                GetCurrentProcess(),
                self.handle,
                GetCurrentProcess(),
                &mut duplicate,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            ) == 0
            {
                return Err(MakxdError::Io(io::Error::last_os_error()));
            }
            let event = match create_io_event() {
                Ok(event) => event,
                Err(error) => {
                    CloseHandle(duplicate);
                    return Err(MakxdError::Io(error));
                }
            };
            Ok(Box::new(Self {
                handle: duplicate,
                event,
            }))
        }
    }
    fn write_coalescing_supported(&self) -> bool {
        true
    }
    fn read_wire(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.io(bytes.as_mut_ptr(), bytes.len() as u32, false)
    }
    fn write_all_wire(&mut self, bytes: &[u8], _response_expected: bool) -> io::Result<()> {
        let mut offset = 0;
        while offset < bytes.len() {
            let count = self.io(
                bytes[offset..].as_ptr() as *mut u8,
                (bytes.len() - offset) as u32,
                true,
            )?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "COM write wrote zero bytes",
                ));
            }
            offset += count;
        }
        Ok(())
    }
    fn drain_wire(&mut self, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            unsafe {
                let mut errors = 0;
                let mut stat: COMSTAT = std::mem::zeroed();
                if ClearCommError(self.handle, &mut errors, &mut stat) == 0 {
                    return Err(io::Error::last_os_error());
                }
                if stat.cbOutQue == 0 {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "COM output queue did not drain",
                ));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::normalized_port_name;

    #[test]
    fn normalizes_com_ports_with_and_without_existing_prefix() {
        assert_eq!(normalized_port_name("COM8"), r"\\.\COM8");
        assert_eq!(normalized_port_name("COM10"), r"\\.\COM10");
        assert_eq!(normalized_port_name(r"\\.\COM10"), r"\\.\COM10");
    }
}
