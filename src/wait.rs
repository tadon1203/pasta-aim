//! The engine thread's wait: it sleeps until a wake-up or a deadline, with a high-resolution timer.
//!
//! The handles are kept as `usize`, so the wrappers are `Send` and `Sync` without an `unsafe impl`.
//! Each Windows call is `unsafe` only to make that call.

use std::{
    ptr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    System::Threading::{
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CancelWaitableTimer, CreateEventW,
        CreateWaitableTimerExW, SetEvent, SetWaitableTimer, TIMER_ALL_ACCESS,
        WaitForMultipleObjects, WaitForSingleObject,
    },
};

/// The timer index in `WaitForMultipleObjects`, after the event.
const TIMER_INDEX: u32 = WAIT_OBJECT_0 + 1;

/// Why a wait returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wake {
    /// The waker was raised. The caller reads its channel.
    Signalled,
    /// The deadline passed with no wake-up.
    Deadline,
}

/// A Windows handle, kept as its address so that the owner is `Send` and `Sync`.
fn raw(handle: usize) -> HANDLE {
    handle as HANDLE
}

/// An auto-reset event. The handle is closed when the last owner drops it.
struct Event(usize);

impl Event {
    fn new() -> Result<Self> {
        let handle = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
        if handle.is_null() {
            bail!(
                "failed to create the engine wake event: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(Self(handle as usize))
    }

    /// Consumes the event when it is raised. Never waits.
    fn take(&self) -> bool {
        unsafe { WaitForSingleObject(raw(self.0), 0) == WAIT_OBJECT_0 }
    }

    /// Waits up to `time` for the event, rounded up to whole milliseconds. Returns `true` when it is raised.
    fn wait_for(&self, time: Duration) -> bool {
        let millis = u32::try_from(time.as_millis().saturating_add(1)).unwrap_or(u32::MAX - 1);
        unsafe { WaitForSingleObject(raw(self.0), millis) == WAIT_OBJECT_0 }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe { CloseHandle(raw(self.0)) };
    }
}

/// The timer of one `DeadlineWait`.
struct Timer(usize);

impl Drop for Timer {
    fn drop(&mut self) {
        unsafe { CloseHandle(raw(self.0)) };
    }
}

/// Raises the wake event of one `DeadlineWait`. Any thread can hold a clone.
#[derive(Clone)]
pub(crate) struct Waker {
    event: Arc<Event>,
}

impl Waker {
    /// The sender sends its message first and calls this second. A wake-up is kept until the next wait.
    pub(crate) fn wake(&self) {
        unsafe { SetEvent(raw(self.event.0)) };
    }
}

/// Waits for a wake-up or a deadline. One thread owns it.
pub(crate) struct DeadlineWait {
    event: Arc<Event>,
    timer: Timer,
}

impl DeadlineWait {
    pub(crate) fn new() -> Result<(Self, Waker)> {
        let event = Arc::new(Event::new()?);
        let mut timer = unsafe {
            CreateWaitableTimerExW(
                ptr::null(),
                ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            )
        };
        if timer.is_null() {
            timer =
                unsafe { CreateWaitableTimerExW(ptr::null(), ptr::null(), 0, TIMER_ALL_ACCESS) };
            if timer.is_null() {
                bail!(
                    "failed to create the engine timer: {}",
                    std::io::Error::last_os_error()
                );
            }
            tracing::warn!("high-resolution timer is not available; the FPS limit is less exact");
        }
        let waker = Waker {
            event: Arc::clone(&event),
        };
        Ok((
            Self {
                event,
                timer: Timer(timer as usize),
            },
            waker,
        ))
    }

    /// Blocks until the waker is raised or `deadline` passes. The last `spin` of the wait is a
    /// busy loop that still returns `Signalled` at once when the waker is raised.
    /// `None` waits for the waker only.
    pub(crate) fn wait_until(&mut self, deadline: Option<Instant>, spin: Duration) -> Wake {
        let Some(deadline) = deadline else {
            unsafe { WaitForSingleObject(raw(self.event.0), u32::MAX) };
            return Wake::Signalled;
        };
        // The timer sleeps until `spin` before the deadline. A timer that fires early is armed again.
        loop {
            let now = Instant::now();
            let spin_from = deadline.checked_sub(spin).unwrap_or(now);
            let left = spin_from.saturating_duration_since(now);
            if left.is_zero() {
                break;
            }
            if !self.arm(left) {
                // Windows refused the timer. The event still ends the wait, so a command is not delayed.
                if self.event.wait_for(left) {
                    return Wake::Signalled;
                }
                continue;
            }
            let handles = [self.event.0, self.timer.0];
            let handles = handles.map(raw);
            match unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, u32::MAX) } {
                // The event is first in the array, so a wake-up wins over a timer that fired too.
                WAIT_OBJECT_0 => {
                    unsafe { CancelWaitableTimer(raw(self.timer.0)) };
                    return Wake::Signalled;
                }
                TIMER_INDEX => {}
                // The wait itself failed. The spin below still meets the deadline.
                _ => break,
            }
        }
        loop {
            if self.event.take() {
                return Wake::Signalled;
            }
            if Instant::now() >= deadline {
                return Wake::Deadline;
            }
        }
    }

    /// Sets the timer to fire after `left`. Returns `false` when Windows refuses it.
    fn arm(&mut self, left: Duration) -> bool {
        // A negative due time is relative, in units of 100 ns.
        let units = i64::try_from(left.as_nanos() / 100)
            .unwrap_or(i64::MAX)
            .max(1);
        let due = -units;
        unsafe { SetWaitableTimer(raw(self.timer.0), &due, 0, None, ptr::null(), 0) != 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wake_up_returns_signalled_before_the_deadline() {
        let (mut wait, waker) = DeadlineWait::new().unwrap();
        let start = Instant::now();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            waker.wake();
        });
        let wake = wait.wait_until(Some(start + Duration::from_secs(5)), Duration::ZERO);
        assert_eq!(wake, Wake::Signalled);
        assert!(start.elapsed() < Duration::from_secs(2));
        sender.join().unwrap();
    }

    #[test]
    fn a_deadline_returns_deadline_not_before_it() {
        let (mut wait, _waker) = DeadlineWait::new().unwrap();
        let deadline = Instant::now() + Duration::from_millis(30);
        let wake = wait.wait_until(Some(deadline), Duration::from_millis(1));
        assert_eq!(wake, Wake::Deadline);
        assert!(Instant::now() >= deadline);
    }

    #[test]
    fn a_past_deadline_returns_at_once() {
        let (mut wait, _waker) = DeadlineWait::new().unwrap();
        let start = Instant::now();
        let wake = wait.wait_until(Some(start), Duration::from_millis(1));
        assert_eq!(wake, Wake::Deadline);
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn a_wake_up_before_the_wait_is_not_lost() {
        let (mut wait, waker) = DeadlineWait::new().unwrap();
        waker.wake();
        let start = Instant::now();
        let wake = wait.wait_until(Some(start + Duration::from_secs(5)), Duration::ZERO);
        assert_eq!(wake, Wake::Signalled);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
