//! Process-wide setup for the app binary.

use std::mem::size_of;

use windows::Win32::{
    System::Threading::{
        GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
        ProcessPowerThrottling, SetProcessInformation,
    },
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
};

/// Opts the whole process out of execution-speed throttling (`EcoQoS`). Best effort: a failure is logged.
pub fn disable_power_throttling() {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        // Control bit set with the state bit clear: throttling is forced off.
        StateMask: 0,
    };
    // SAFETY: `state` outlives the call and the size matches the information class.
    let result = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            std::ptr::from_ref(&state).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    if let Err(error) = result {
        tracing::warn!(%error, "could not disable process power throttling");
    }
}

/// Sets per-monitor-v2 DPI awareness for the process. Call it before any window is created.
/// Returns `false` when Windows refuses it.
pub fn enable_per_monitor_dpi_awareness() -> bool {
    // SAFETY: this sets process-wide DPI behavior before creating any windows.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }.is_ok()
}
