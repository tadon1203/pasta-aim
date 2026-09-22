mod capture;

pub use capture::DxgiCapture;
pub(crate) use capture::MappedCaptureFrame;

use std::{marker::PhantomData, rc::Rc};

use anyhow::{Context, Result, anyhow};
use win_desktop_duplication::devices::AdapterFactory;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

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

    pub(crate) fn enumerate_adapters(&self) -> Vec<AdapterInfo> {
        let adapters = enumerate_adapters_impl();
        tracing::debug!(count = adapters.len(), "enumerated graphics adapters");
        adapters
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInfo {
    pub index: u32,
    pub name: String,
}

impl AdapterInfo {
    pub fn label(&self) -> String {
        format!("Device {} · {}", self.index, self.name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayInfo {
    pub adapter_index: u32,
    pub output_index: u32,
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

fn enumerate_adapters_impl() -> Vec<AdapterInfo> {
    let factory = AdapterFactory::new();
    let mut result = Vec::new();
    for index in 0.. {
        let Some(adapter) = factory.get_adapter_by_idx(index) else {
            break;
        };
        result.push(AdapterInfo {
            index,
            name: adapter.name(),
        });
    }
    result
}

fn enumerate_displays_impl() -> Result<Vec<DisplayInfo>> {
    let factory = AdapterFactory::new();
    let mut result = Vec::new();

    for adapter_index in 0.. {
        let Some(adapter) = factory.get_adapter_by_idx(adapter_index) else {
            break;
        };
        let adapter_name = adapter.name();
        for output_index in 0.. {
            let Some(display) = adapter.get_display_by_idx(output_index) else {
                break;
            };
            let mode = display
                .get_current_display_mode()
                .map_err(|error| anyhow!("failed to query display mode: {error:?}"))?;
            let refresh_hz = if mode.refresh_den == 0 {
                0.0
            } else {
                f64::from(mode.refresh_num) / f64::from(mode.refresh_den)
            };
            result.push(DisplayInfo {
                adapter_index,
                output_index,
                adapter_name: adapter_name.clone(),
                display_name: display.name(),
                width: mode.width,
                height: mode.height,
                refresh_hz,
            });
        }
    }

    Ok(result)
}
