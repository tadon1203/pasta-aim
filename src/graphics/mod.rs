pub mod bootstrap;
mod capture;
pub mod device;
pub mod frame_mailbox;
pub mod gpu_diagnostics;
pub mod preprocess;
pub mod shared_frame_pool;

pub use capture::DxgiCapture;
pub(crate) use capture::{CapturePoll, GpuCopySample, NoDesktopUpdateReason};
pub use frame_mailbox::{
    FrameConsumer, FramePublisher, GpuFrameMetadata, frame_mailbox,
};
pub use gpu_diagnostics::{GpuDropCounts, GpuDropReason, GpuFaultReport, GpuFaultStage};
pub use shared_frame_pool::CaptureFramePool;

use std::{marker::PhantomData, rc::Rc};

use crate::inference::AdapterLuid;
use anyhow::{Context, Result, anyhow};
use windows::{
    Win32::{
        Graphics::{
            Dxgi::{
                CreateDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ERROR_NOT_FOUND, DXGI_OUTPUT_DESC,
                IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
            },
            Gdi::{
                DEVMODEW, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS,
                EnumDisplaySettingsExW,
            },
        },
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
    },
    core::{Interface, PCWSTR},
};

/// Owns COM and DXGI initialization for the dedicated graphics thread.
///
/// The token is deliberately !Send/!Sync: all DXGI enumeration and capture
/// creation must stay on the thread that initialized COM.
pub(crate) struct GraphicsThread(PhantomData<Rc<()>>);

impl GraphicsThread {
    pub(crate) fn initialize() -> Result<Self> {
        // SAFETY: the engine worker is a dedicated thread. MTA is selected
        // once for its entire lifetime and balanced by Drop below.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .context("failed to initialize COM MTA on the graphics thread")?;
        tracing::debug!("graphics thread COM MTA initialized");
        Ok(Self(PhantomData))
    }

    pub(crate) fn enumerate_displays(&self) -> Result<Vec<DisplayInfo>> {
        let displays = enumerate_displays_impl()?;
        tracing::debug!(count = displays.len(), "enumerated displays");
        Ok(displays)
    }

    pub(crate) fn create_capture(
        &self,
        adapter_index: u32,
        output_index: u32,
    ) -> Result<DxgiCapture> {
        tracing::debug!(adapter_index, output_index, "creating DXGI capture");
        DxgiCapture::new(adapter_index, output_index)
    }
}

impl Drop for GraphicsThread {
    fn drop(&mut self) {
        tracing::debug!("graphics thread COM MTA shutting down");
        // SAFETY: GraphicsThread is !Send/!Sync, so this runs on the same
        // thread that successfully called CoInitializeEx.
        unsafe { CoUninitialize() };
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayInfo {
    pub adapter_index: u32,
    pub output_index: u32,
    pub adapter_luid: AdapterLuid,
    pub adapter_name: String,
    pub display_name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
}

impl DisplayInfo {
    pub fn label(&self) -> String {
        format!(
            "{} · {} · {}×{} @ {:.1} Hz",
            self.adapter_name, self.display_name, self.width, self.height, self.refresh_hz
        )
    }
}

pub(super) struct SelectedOutput {
    pub adapter: IDXGIAdapter1,
    pub output: IDXGIOutput1,
    pub info: DisplayInfo,
}

fn create_factory() -> Result<IDXGIFactory1> {
    // SAFETY: CreateDXGIFactory1 initializes and returns the requested COM interface.
    unsafe { CreateDXGIFactory1() }.context("failed to create DXGI factory")
}

fn select_output(adapter_index: u32, output_index: u32) -> Result<SelectedOutput> {
    let factory = create_factory()?;
    let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
        Ok(adapter) => adapter,
        Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => {
            return Err(anyhow!("no DXGI adapter {adapter_index}"));
        }
        Err(error) => return Err(error).context("failed to enumerate DXGI adapter"),
    };
    let output = match unsafe { adapter.EnumOutputs(output_index) } {
        Ok(output) => output,
        Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => {
            return Err(anyhow!(
                "no DXGI output {output_index} on adapter {adapter_index}"
            ));
        }
        Err(error) => return Err(error).context("failed to enumerate DXGI output"),
    };
    let info = current_display_info(adapter_index, output_index, &adapter, &output)?;
    let output = output
        .cast::<IDXGIOutput1>()
        .context("selected DXGI output does not support IDXGIOutput1")?;
    Ok(SelectedOutput {
        adapter,
        output,
        info,
    })
}

fn current_display_info(
    adapter_index: u32,
    output_index: u32,
    adapter: &IDXGIAdapter1,
    output: &IDXGIOutput,
) -> Result<DisplayInfo> {
    let mut adapter_desc = DXGI_ADAPTER_DESC1::default();
    // SAFETY: the descriptor points to writable storage of the documented type.
    unsafe { adapter.GetDesc1(&mut adapter_desc) }.context("failed to query DXGI adapter")?;

    let mut output_desc = DXGI_OUTPUT_DESC::default();
    // SAFETY: the descriptor points to writable storage of the documented type.
    unsafe { output.GetDesc(&mut output_desc) }.context("failed to query DXGI output")?;

    let mut mode = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: DeviceName is null-terminated by DXGI and mode is initialized with its size.
    let mode_found = unsafe {
        EnumDisplaySettingsExW(
            PCWSTR(output_desc.DeviceName.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut mode,
            ENUM_DISPLAY_SETTINGS_FLAGS(0),
        )
    };
    if !mode_found.as_bool() {
        return Err(anyhow!(
            "failed to query current display mode for {}",
            wide_string(&output_desc.DeviceName)
        ));
    }

    Ok(DisplayInfo {
        adapter_index,
        output_index,
        adapter_luid: AdapterLuid {
            low: adapter_desc.AdapterLuid.LowPart,
            high: adapter_desc.AdapterLuid.HighPart,
        },
        adapter_name: wide_string(&adapter_desc.Description),
        display_name: wide_string(&output_desc.DeviceName),
        width: mode.dmPelsWidth,
        height: mode.dmPelsHeight,
        refresh_hz: f64::from(mode.dmDisplayFrequency),
    })
}

fn enumerate_displays_impl() -> Result<Vec<DisplayInfo>> {
    let factory = create_factory()?;
    let mut result = Vec::new();
    for adapter_index in 0.. {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => return Err(error).context("failed to enumerate DXGI adapters"),
        };
        for output_index in 0.. {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(error).context("failed to enumerate DXGI outputs"),
            };
            result.push(current_display_info(
                adapter_index,
                output_index,
                &adapter,
                &output,
            )?);
        }
    }
    Ok(result)
}

fn wide_string(value: &[u16]) -> String {
    let end = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}
