//! Process and thread scheduling hints.
//!
//! While a game is in the foreground this process counts as background work:
//! Windows may throttle it (`EcoQoS`, efficiency cores) and its threads compete
//! with the game at normal priority. These calls opt the latency-critical
//! threads out. All of them are best effort; failures are logged and ignored.

use std::mem::size_of;

use windows::Win32::{
    Foundation::HANDLE,
    System::Threading::{
        AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, GetCurrentProcess,
        GetCurrentThread, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
        ProcessPowerThrottling, SetProcessInformation, SetThreadInformation, SetThreadPriority,
        THREAD_POWER_THROTTLING_CURRENT_VERSION, THREAD_POWER_THROTTLING_EXECUTION_SPEED,
        THREAD_POWER_THROTTLING_STATE, THREAD_PRIORITY_HIGHEST, ThreadPowerThrottling,
    },
};
use windows::core::w;

/// Opts the whole process out of execution-speed throttling (`EcoQoS`).
pub fn disable_process_power_throttling() {
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
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    // SAFETY: this sets process-wide DPI behavior before creating any windows.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }.is_ok()
}

/// Keeps a thread's scheduling boost for as long as it is alive.
pub(crate) struct ThreadBoost {
    mmcss: Option<HANDLE>,
}

impl Drop for ThreadBoost {
    fn drop(&mut self) {
        if let Some(handle) = self.mmcss {
            // SAFETY: the handle came from AvSetMmThreadCharacteristicsW on this thread.
            let _ = unsafe { AvRevertMmThreadCharacteristics(handle) };
        }
    }
}

/// Raises the calling thread: higher priority, no power throttling, and an
/// MMCSS registration so it is scheduled ahead of ordinary background work.
///
/// Hold the returned guard for the lifetime of the thread.
pub(crate) fn boost_current_thread(name: &str) -> ThreadBoost {
    // SAFETY: pseudo-handle for the calling thread; no ownership is transferred.
    let thread = unsafe { GetCurrentThread() };
    if let Err(error) = unsafe { SetThreadPriority(thread, THREAD_PRIORITY_HIGHEST) } {
        tracing::warn!(thread = name, %error, "could not raise thread priority");
    }
    let throttling = THREAD_POWER_THROTTLING_STATE {
        Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: 0,
    };
    // SAFETY: `throttling` outlives the call and the size matches the information class.
    let result = unsafe {
        SetThreadInformation(
            thread,
            ThreadPowerThrottling,
            std::ptr::from_ref(&throttling).cast(),
            size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
        )
    };
    if let Err(error) = result {
        tracing::warn!(thread = name, %error, "could not disable thread power throttling");
    }
    let mut task_index = 0_u32;
    // SAFETY: the task name is a static wide string and `task_index` is writable.
    let mmcss = match unsafe { AvSetMmThreadCharacteristicsW(w!("Games"), &mut task_index) } {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::warn!(thread = name, %error, "could not register thread with MMCSS");
            None
        }
    };
    tracing::debug!(
        thread = name,
        mmcss = mmcss.is_some(),
        "thread scheduling boosted"
    );
    ThreadBoost { mmcss }
}
