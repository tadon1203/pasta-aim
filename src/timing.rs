use anyhow::{Context, Result};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

#[derive(Debug, Clone, Copy)]
pub(crate) struct QpcClock {
    ticks_per_second: f64,
}

impl QpcClock {
    pub(crate) fn new() -> Result<Self> {
        let mut frequency = 0i64;
        // SAFETY: Windows writes the performance-counter frequency to valid storage.
        unsafe { QueryPerformanceFrequency(&mut frequency) }
            .context("QueryPerformanceFrequency failed")?;
        if frequency <= 0 {
            anyhow::bail!("QueryPerformanceFrequency returned an invalid frequency");
        }
        Ok(Self {
            ticks_per_second: frequency as f64,
        })
    }

    pub(crate) fn now() -> i64 {
        let mut counter = 0i64;
        // SAFETY: Windows writes the current performance counter to valid storage.
        let ok = unsafe { QueryPerformanceCounter(&mut counter) };
        debug_assert!(ok.is_ok());
        counter
    }

    pub(crate) fn frequency_hz(self) -> i64 {
        self.ticks_per_second as i64
    }

    pub(crate) fn elapsed_ms(self, start_qpc: i64, end_qpc: i64) -> f64 {
        (end_qpc - start_qpc) as f64 * 1000.0 / self.ticks_per_second
    }

    #[cfg(test)]
    pub(crate) const fn test(ticks_per_second: f64) -> Self {
        Self { ticks_per_second }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_ms_uses_qpc_delta() {
        let clock = QpcClock {
            ticks_per_second: 10_000.0,
        };
        assert_eq!(clock.elapsed_ms(1_000, 11_000), 1_000.0);
    }
}
