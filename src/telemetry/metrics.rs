use std::collections::VecDeque;

use crate::timing::QpcClock;

const MAX_SAMPLES: usize = 256;

/// The stage times of one frame, in milliseconds.
#[derive(Debug, Clone, Copy, Default)]
pub struct StageTimings {
    pub present_to_gpu_submit_ms: f64,
    pub present_to_inference_start_ms: f64,
    pub acquire_wait_ms: f64,
    pub capture_ms: f64,
    pub copy_submit_ms: f64,
    /// Only measured on some frames.
    pub gpu_preprocess_ms: Option<f64>,
    pub inference_ms: f64,
    pub postprocess_ms: f64,
    pub makcu_write_ms: f64,
    pub total_ms: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CaptureSourceMetrics {
    pub fresh_fps: f64,
    pub processed_fps: f64,
    pub timeouts: u64,
    pub mean_accumulated_frames: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MetricSummary {
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PerformanceSnapshot {
    pub sample_count: usize,
    pub fps: f64,
    pub cadence: MetricSummary,
    pub present_to_gpu_submit: MetricSummary,
    pub present_to_inference_start: MetricSummary,
    pub acquire_wait: MetricSummary,
    pub capture: MetricSummary,
    pub copy_submit: MetricSummary,
    pub gpu_copy: MetricSummary,
    pub preprocess: MetricSummary,
    pub inference: MetricSummary,
    pub postprocess: MetricSummary,
    pub makcu_write: MetricSummary,
    pub total: MetricSummary,
    pub capture_source: CaptureSourceMetrics,
    pub cold_run: Option<StageTimings>,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    timings: StageTimings,
    cadence_ms: Option<f64>,
}

/// The rolling statistics of the Frame records. The percentiles are made in `snapshot`.
pub struct Metrics {
    samples: VecDeque<Sample>,
    gpu_copy_samples: VecDeque<f64>,
    capture_source: CaptureSourceMetrics,
    cold_run: Option<StageTimings>,
    cold_next: bool,
    previous_started_qpc: Option<i64>,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(MAX_SAMPLES),
            gpu_copy_samples: VecDeque::with_capacity(MAX_SAMPLES),
            capture_source: CaptureSourceMetrics::default(),
            cold_run: None,
            cold_next: true,
            previous_started_qpc: None,
        }
    }

    pub fn mark_cold_next(&mut self) {
        self.samples.clear();
        self.gpu_copy_samples.clear();
        self.capture_source = CaptureSourceMetrics::default();
        self.cold_run = None;
        self.cold_next = true;
        self.previous_started_qpc = None;
    }

    pub fn record_qpc(&mut self, qpc: QpcClock, started_qpc: i64, timings: StageTimings) {
        if self.cold_next {
            self.cold_run = Some(timings);
            self.cold_next = false;
            self.previous_started_qpc = None;
            return;
        }

        let cadence_ms = self
            .previous_started_qpc
            .map(|previous| qpc.elapsed_ms(previous, started_qpc));
        self.previous_started_qpc = Some(started_qpc);
        if self.samples.len() == MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample {
            timings,
            cadence_ms,
        });
    }

    pub fn update_capture_source(&mut self, value: CaptureSourceMetrics) {
        self.capture_source = value;
    }

    pub fn record_gpu_copy(&mut self, gpu_copy_ms: f64) {
        if self.gpu_copy_samples.len() == MAX_SAMPLES {
            self.gpu_copy_samples.pop_front();
        }
        self.gpu_copy_samples.push_back(gpu_copy_ms);
    }

    pub fn snapshot(&self) -> PerformanceSnapshot {
        let mut scratch = [0.0_f64; MAX_SAMPLES];
        let (cadence_sum, cadence_count) = self
            .samples
            .iter()
            .filter_map(|sample| sample.cadence_ms)
            .fold((0.0, 0), |(sum, count), value| (sum + value, count + 1));
        let mean_cadence = if cadence_count == 0 {
            None
        } else {
            Some(cadence_sum / cadence_count as f64)
        };
        let cadence = summarize_samples(&self.samples, &mut scratch, |sample| sample.cadence_ms);

        PerformanceSnapshot {
            sample_count: self.samples.len(),
            fps: mean_cadence
                .filter(|value| *value > 0.0)
                .map(|value| 1000.0 / value)
                .unwrap_or(0.0),
            cadence,
            present_to_gpu_submit: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.present_to_gpu_submit_ms)
            }),
            present_to_inference_start: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.present_to_inference_start_ms)
            }),
            acquire_wait: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.acquire_wait_ms)
            }),
            capture: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.capture_ms)
            }),
            copy_submit: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.copy_submit_ms)
            }),
            gpu_copy: summarize_values(&self.gpu_copy_samples, &mut scratch),
            preprocess: summarize_samples(&self.samples, &mut scratch, |sample| {
                sample.timings.gpu_preprocess_ms
            }),
            inference: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.inference_ms)
            }),
            postprocess: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.postprocess_ms)
            }),
            makcu_write: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.makcu_write_ms)
            }),
            total: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.total_ms)
            }),
            capture_source: self.capture_source,
            cold_run: self.cold_run,
        }
    }
}

fn summarize_values(values: &VecDeque<f64>, scratch: &mut [f64; MAX_SAMPLES]) -> MetricSummary {
    let len = values.len();
    for (index, value) in values.iter().enumerate() {
        scratch[index] = *value;
    }
    summarize_scratch(&mut scratch[..len])
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

fn summarize_samples(
    samples: &VecDeque<Sample>,
    scratch: &mut [f64; MAX_SAMPLES],
    select: impl Fn(&Sample) -> Option<f64>,
) -> MetricSummary {
    let mut len = 0;
    for sample in samples {
        if let Some(value) = select(sample) {
            scratch[len] = value;
            len += 1;
        }
    }
    summarize_scratch(&mut scratch[..len])
}

/// The p50, p95, and p99 of `values`, which the selection reorders.
fn summarize_scratch(values: &mut [f64]) -> MetricSummary {
    if values.is_empty() {
        return MetricSummary::default();
    }
    MetricSummary {
        p50_ms: percentile(values, 0.50),
        p95_ms: percentile(values, 0.95),
        p99_ms: percentile(values, 0.99),
    }
}

/// The value at `fraction` in sorted order. It selects that value without a full sort.
fn percentile(values: &mut [f64], fraction: f64) -> f64 {
    let index = ((values.len() - 1) as f64 * fraction).round() as usize;
    *values.select_nth_unstable_by(index, f64::total_cmp).1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_is_kept_as_cold_run() {
        let mut metrics = Metrics::new();
        let qpc = QpcClock::test(1_000.0);
        metrics.record_qpc(
            qpc,
            0,
            StageTimings {
                total_ms: 12.0,
                ..Default::default()
            },
        );

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.sample_count, 0);
        assert_eq!(snapshot.cold_run.expect("cold run").total_ms, 12.0);
    }

    #[test]
    fn percentile_summary_uses_rolling_samples() {
        let mut metrics = Metrics::new();
        let qpc = QpcClock::test(1_000.0);
        metrics.record_qpc(qpc, 0, StageTimings::default());
        for i in 1..=20 {
            metrics.record_qpc(
                qpc,
                i * 10,
                StageTimings {
                    total_ms: i as f64,
                    ..Default::default()
                },
            );
        }

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.sample_count, 20);
        assert_eq!(snapshot.total.p50_ms, 11.0);
        assert_eq!(snapshot.total.p95_ms, 19.0);
        assert_eq!(snapshot.cadence.p50_ms, 10.0);
        assert_eq!(snapshot.cadence.p95_ms, 10.0);
        assert_eq!(snapshot.fps, 100.0);
    }
}
