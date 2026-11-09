use std::mem::size_of;

use windows::{
    Win32::{
        Foundation::HANDLE,
        System::Threading::{
            AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, GetCurrentThread,
            SetThreadInformation, SetThreadPriority, THREAD_POWER_THROTTLING_CURRENT_VERSION,
            THREAD_POWER_THROTTLING_EXECUTION_SPEED, THREAD_POWER_THROTTLING_STATE,
            THREAD_PRIORITY_HIGHEST, ThreadPowerThrottling,
        },
    },
    core::w,
};

pub(super) struct ThreadBoost {
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

/// Hold the returned guard for the lifetime of the thread.
pub(super) fn boost_current_thread(name: &str) -> ThreadBoost {
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
