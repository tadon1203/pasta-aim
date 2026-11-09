use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        System::{
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
            Threading::{
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CancelWaitableTimer, CreateEventW,
                CreateWaitableTimerExW, SetEvent, SetWaitableTimer, TIMER_ALL_ACCESS,
                WaitForMultipleObjects, WaitForSingleObject,
            },
        },
    },
    core::PCWSTR,
};

const TIMER_INDEX: u32 = WAIT_OBJECT_0.0 + 1;

#[derive(Debug, Clone, Copy)]
pub struct Clock {
    ticks_per_second: f64,
}

impl Clock {
    pub fn new() -> Result<Self> {
        let mut frequency = 0i64;
        // SAFETY: Windows writes the performance-counter frequency to valid storage.
        unsafe { QueryPerformanceFrequency(&mut frequency) }
            .context("QueryPerformanceFrequency failed")?;
        if frequency <= 0 {
            bail!("QueryPerformanceFrequency returned an invalid frequency");
        }
        Ok(Self {
            ticks_per_second: frequency as f64,
        })
    }

    pub fn now() -> i64 {
        let mut counter = 0i64;
        // SAFETY: Windows writes the current performance counter to valid storage.
        let ok = unsafe { QueryPerformanceCounter(&mut counter) };
        debug_assert!(ok.is_ok());
        counter
    }

    pub fn ms(self, from: i64, to: i64) -> f64 {
        (to - from) as f64 * 1000.0 / self.ticks_per_second
    }

    #[cfg(test)]
    pub(crate) const fn test(ticks_per_second: f64) -> Self {
        Self { ticks_per_second }
    }
}

#[derive(Debug)]
struct Handle(isize);

impl Handle {
    fn raw(&self) -> HANDLE {
        HANDLE(self.0)
    }

    fn take(&self) -> bool {
        unsafe { WaitForSingleObject(self.raw(), 0) == WAIT_OBJECT_0 }
    }

    fn wait_for(&self, time: Duration) -> bool {
        let millis = u32::try_from(time.as_millis().saturating_add(1)).unwrap_or(u32::MAX - 1);
        unsafe { WaitForSingleObject(self.raw(), millis) == WAIT_OBJECT_0 }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.raw()) };
    }
}

#[derive(Debug, Clone)]
pub struct Wake {
    event: Arc<Handle>,
}

impl Wake {
    /// Call this after the message is sent. A wake-up is kept until the next wait.
    pub fn wake(&self) {
        let _ = unsafe { SetEvent(self.event.raw()) };
    }
}

#[derive(Debug)]
pub struct Wait {
    event: Arc<Handle>,
    timer: Handle,
}

pub fn wait() -> Result<(Wait, Wake)> {
    // SAFETY: the event has no name and no security attributes; the result is checked below.
    let event = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
        .context("failed to create the wake event")?;
    let event = Arc::new(Handle(event.0));
    let mut timer = unsafe {
        CreateWaitableTimerExW(
            None,
            PCWSTR::null(),
            CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
            TIMER_ALL_ACCESS.0,
        )
    };
    if timer.is_err() {
        tracing::warn!("high-resolution timer is not available; the FPS limit is less exact");
        timer = unsafe { CreateWaitableTimerExW(None, PCWSTR::null(), 0, TIMER_ALL_ACCESS.0) };
    }
    let timer = timer.context("failed to create the timer")?;
    Ok((
        Wait {
            event: Arc::clone(&event),
            timer: Handle(timer.0),
        },
        Wake { event },
    ))
}

impl Wait {
    pub fn until(&mut self, deadline: Option<Instant>, spin: Duration) -> bool {
        let Some(deadline) = deadline else {
            unsafe { WaitForSingleObject(self.event.raw(), u32::MAX) };
            return true;
        };
        loop {
            let now = Instant::now();
            let spin_from = deadline.checked_sub(spin).unwrap_or(now);
            let left = spin_from.saturating_duration_since(now);
            if left.is_zero() {
                break;
            }
            if !self.arm(left) {
                if self.event.wait_for(left) {
                    return true;
                }
                continue;
            }
            let handles = [self.event.raw(), self.timer.raw()];
            match unsafe { WaitForMultipleObjects(&handles, false, u32::MAX) } {
                // The event is first so that a wake-up wins over a timer that fired too.
                WAIT_OBJECT_0 => {
                    let _ = unsafe { CancelWaitableTimer(self.timer.raw()) };
                    return true;
                }
                status if status.0 == TIMER_INDEX => {}
                _ => break,
            }
        }
        loop {
            if self.event.take() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
        }
    }

    fn arm(&mut self, left: Duration) -> bool {
        // A negative due time is relative, in units of 100 ns.
        let units = i64::try_from(left.as_nanos() / 100)
            .unwrap_or(i64::MAX)
            .max(1);
        let due = -units;
        unsafe { SetWaitableTimer(self.timer.raw(), &due, 0, None, None, false) }.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_uses_the_tick_delta() {
        let clock = Clock::test(10_000.0);
        assert_eq!(clock.ms(1_000, 11_000), 1_000.0);
    }

    #[test]
    fn a_wake_up_returns_true_before_the_deadline() {
        let (mut wait, wake) = wait().unwrap();
        let start = Instant::now();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            wake.wake();
        });
        assert!(wait.until(Some(start + Duration::from_secs(5)), Duration::ZERO));
        assert!(start.elapsed() < Duration::from_secs(2));
        sender.join().unwrap();
    }

    #[test]
    fn a_deadline_returns_false_not_before_it() {
        let (mut wait, _wake) = wait().unwrap();
        let deadline = Instant::now() + Duration::from_millis(30);
        assert!(!wait.until(Some(deadline), Duration::from_millis(1)));
        assert!(Instant::now() >= deadline);
    }

    #[test]
    fn a_past_deadline_returns_at_once() {
        let (mut wait, _wake) = wait().unwrap();
        let start = Instant::now();
        assert!(!wait.until(Some(start), Duration::from_millis(1)));
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn a_wake_up_before_the_wait_is_not_lost() {
        let (mut wait, wake) = wait().unwrap();
        wake.wake();
        let start = Instant::now();
        assert!(wait.until(Some(start + Duration::from_secs(5)), Duration::ZERO));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
