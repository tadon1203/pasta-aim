//! Tracking-loop quality metrics derived from the frame records.

use std::collections::VecDeque;

use crate::engine::target_track::Track;
use crate::{clock::Clock, stats::percentile};

const WINDOW: usize = 256;
const CONVERGENCE_WINDOW: usize = 64;
/// Sign flips with either side smaller than this are noise, not overshoot.
const OVERSHOOT_DEADBAND_PX: f32 = 2.0;
/// The target counts as reached once the error is below this.
const CONVERGED_PX: f32 = 6.0;

#[derive(Debug, Clone, Copy, Default)]
pub struct LoopSnapshot {
    pub samples: usize,
    pub period_p50_ms: f64,
    pub period_p95_ms: f64,
    pub rms_error_px: f64,
    pub overshoots: u64,
    pub locked_frames: u64,
    pub convergence_last_ms: Option<f64>,
    pub convergence_p50_ms: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LoopFrame {
    pub present_qpc: i64,
    pub output_complete_qpc: i64,
    pub track: Option<Track>,
    /// Error to the aim point, in model input pixels.
    pub error: Option<(f32, f32)>,
    pub sent: bool,
}

pub(super) struct LoopMetrics {
    periods_ms: VecDeque<f64>,
    squared_errors: VecDeque<f32>,
    convergence_ms: VecDeque<f64>,
    last_send_qpc: Option<i64>,
    previous_error: Option<(f32, f32)>,
    acquired_qpc: i64,
    converged: bool,
    overshoots: u64,
    locked_frames: u64,
}

impl LoopMetrics {
    pub(super) fn new() -> Self {
        Self {
            periods_ms: VecDeque::new(),
            squared_errors: VecDeque::new(),
            convergence_ms: VecDeque::new(),
            last_send_qpc: None,
            previous_error: None,
            acquired_qpc: 0,
            converged: true,
            overshoots: 0,
            locked_frames: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        *self = Self::new();
    }

    pub(super) fn record(&mut self, qpc: Clock, frame: LoopFrame) {
        if frame.sent {
            if let Some(previous) = self.last_send_qpc {
                push(
                    &mut self.periods_ms,
                    qpc.ms(previous, frame.output_complete_qpc),
                    WINDOW,
                );
            }
            self.last_send_qpc = Some(frame.output_complete_qpc);
        }

        let (Some(track), Some(error)) = (frame.track, frame.error) else {
            self.previous_error = None;
            return;
        };
        if track.switched {
            self.acquired_qpc = frame.present_qpc;
            self.converged = false;
            self.previous_error = None;
        } else {
            self.locked_frames += 1;
            push(
                &mut self.squared_errors,
                error.0 * error.0 + error.1 * error.1,
                WINDOW,
            );
            if let Some(previous) = self.previous_error
                && (flipped(previous.0, error.0) || flipped(previous.1, error.1))
            {
                self.overshoots += 1;
            }
        }
        if !self.converged && error.0.hypot(error.1) < CONVERGED_PX {
            self.converged = true;
            push(
                &mut self.convergence_ms,
                qpc.ms(self.acquired_qpc, frame.present_qpc),
                CONVERGENCE_WINDOW,
            );
        }
        self.previous_error = Some(error);
    }

    /// Made when the live values are read: the percentiles select from a copy of the windows.
    pub(super) fn snapshot(&self) -> LoopSnapshot {
        let mut periods: Vec<f64> = self.periods_ms.iter().copied().collect();
        let mut convergence: Vec<f64> = self.convergence_ms.iter().copied().collect();
        let rms_error_px = if self.squared_errors.is_empty() {
            0.0
        } else {
            let sum: f64 = self.squared_errors.iter().map(|&e| f64::from(e)).sum();
            (sum / self.squared_errors.len() as f64).sqrt()
        };
        LoopSnapshot {
            samples: self.periods_ms.len(),
            period_p50_ms: percentile(&mut periods, 50).unwrap_or(0.0),
            period_p95_ms: percentile(&mut periods, 95).unwrap_or(0.0),
            rms_error_px,
            overshoots: self.overshoots,
            locked_frames: self.locked_frames,
            convergence_last_ms: self.convergence_ms.back().copied(),
            convergence_p50_ms: percentile(&mut convergence, 50),
        }
    }
}

fn flipped(previous: f32, current: f32) -> bool {
    previous.abs() > OVERSHOOT_DEADBAND_PX
        && current.abs() > OVERSHOOT_DEADBAND_PX
        && (previous > 0.0) != (current > 0.0)
}

fn push<T>(queue: &mut VecDeque<T>, value: T, capacity: usize) {
    if queue.len() == capacity {
        queue.pop_front();
    }
    queue.push_back(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    const QPC: Clock = Clock::test(1_000.0);

    fn frame(t: i64, id: u32, switched: bool, error: (f32, f32)) -> LoopFrame {
        LoopFrame {
            present_qpc: t,
            output_complete_qpc: t + 5,
            track: Some(Track { id, switched }),
            error: Some(error),
            sent: true,
        }
    }

    #[test]
    fn convergence_is_measured_from_acquisition_and_switch_frames_are_not_locked() {
        let mut metrics = LoopMetrics::new();
        metrics.record(QPC, frame(0, 1, true, (100.0, 0.0)));
        metrics.record(QPC, frame(30, 1, false, (40.0, 0.0)));
        metrics.record(QPC, frame(60, 1, false, (4.0, 0.0)));
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.convergence_last_ms, Some(60.0));
        assert_eq!(snapshot.locked_frames, 2);
        assert!((snapshot.period_p50_ms - 30.0).abs() < 1e-9);
    }

    #[test]
    fn overshoot_counts_sign_flips_outside_the_deadband() {
        let mut metrics = LoopMetrics::new();
        metrics.record(QPC, frame(0, 1, true, (30.0, 0.0)));
        metrics.record(QPC, frame(30, 1, false, (10.0, 0.0)));
        metrics.record(QPC, frame(60, 1, false, (-8.0, 0.0)));
        // A flip inside the deadband is noise.
        metrics.record(QPC, frame(90, 1, false, (1.0, 0.0)));
        metrics.record(QPC, frame(120, 1, false, (-1.0, 0.0)));
        assert_eq!(metrics.snapshot().overshoots, 1);
    }

    #[test]
    fn rms_error_is_reported() {
        let mut metrics = LoopMetrics::new();
        metrics.record(QPC, frame(0, 1, true, (0.0, 0.0)));
        metrics.record(QPC, frame(30, 1, false, (3.0, 4.0)));
        let snapshot = metrics.snapshot();
        assert!((snapshot.rms_error_px - 5.0).abs() < 1e-9);
    }
}
