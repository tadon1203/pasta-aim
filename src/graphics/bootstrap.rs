use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_DRED_ENABLEMENT_FORCED_ON, D3D12GetDebugInterface, ID3D12Debug,
    ID3D12DeviceRemovedExtendedDataSettings,
};

use crate::config::LogLevel;

static INITIALIZED: OnceLock<std::result::Result<(), String>> = OnceLock::new();

/// Enables process-wide D3D12 diagnostics before any D3D12 device is created.
pub fn initialize_graphics_diagnostics(level: LogLevel) -> Result<()> {
    let initialized = INITIALIZED.get_or_init(|| {
        let enabled = matches!(level, LogLevel::Debug | LogLevel::Trace);
        if !enabled {
            return Ok(());
        }

        let mut debug: Option<ID3D12Debug> = None;
        unsafe { D3D12GetDebugInterface(&mut debug) }
            .context("failed to get D3D12 debug interface")
            .map_err(|error| format!("{error:#}"))?;
        let debug: ID3D12Debug =
            debug.ok_or_else(|| "D3D12 debug interface was empty".to_owned())?;
        unsafe { debug.EnableDebugLayer() };

        let mut dred: Option<ID3D12DeviceRemovedExtendedDataSettings> = None;
        unsafe { D3D12GetDebugInterface(&mut dred) }
            .context("failed to get DRED settings interface")
            .map_err(|error| format!("{error:#}"))?;
        let dred: ID3D12DeviceRemovedExtendedDataSettings =
            dred.ok_or_else(|| "DRED settings interface was empty".to_owned())?;
        unsafe {
            dred.SetAutoBreadcrumbsEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
            dred.SetPageFaultEnablement(D3D12_DRED_ENABLEMENT_FORCED_ON);
        }
        tracing::info!("D3D12 debug layer and DRED enabled");
        Ok(())
    });

    initialized
        .as_ref()
        .map(|_| ())
        .map_err(|error| anyhow!(error.clone()))
}
