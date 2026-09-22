use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const MAX_SAMPLES: usize = 256;
const PUBLISH_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, Default)]
pub struct StageTimings {
    pub frame_wait_ms: f64,
    pub capture_ms: f64,
    pub preprocess_ms: f64,
    pub inference_ms: f64,
    pub postprocess_ms: f64,
    pub mak_write_ms: f64,
    pub total_ms: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MetricSummary {
    pub p50_ms: f64,
    pub p95_ms: f64,
}

#[derive(Debug, Clone, Default)]
pub struct PerformanceSnapshot {
    pub sample_count: usize,
    pub fps: f64,
    pub cadence: MetricSummary,
    pub frame_wait: MetricSummary,
    pub capture: MetricSummary,
    pub preprocess: MetricSummary,
    pub inference: MetricSummary,
    pub postprocess: MetricSummary,
    pub mak_write: MetricSummary,
    pub total: MetricSummary,
    pub cold_run: Option<StageTimings>,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    timings: StageTimings,
    cadence_ms: Option<f64>,
}

pub struct Metrics {
    samples: VecDeque<Sample>,
    cold_run: Option<StageTimings>,
    cold_next: bool,
    previous_started: Option<Instant>,
    last_publish: Instant,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(MAX_SAMPLES),
            cold_run: None,
            cold_next: true,
            previous_started: None,
            last_publish: Instant::now(),
        }
    }

    pub fn mark_cold_next(&mut self) {
        self.samples.clear();
        self.cold_run = None;
        self.cold_next = true;
        self.previous_started = None;
    }

    pub fn record(&mut self, started: Instant, timings: StageTimings) {
        if self.cold_next {
            self.cold_run = Some(timings);
            self.cold_next = false;
            self.previous_started = None;
            return;
        }

        let cadence_ms = self
            .previous_started
            .map(|previous| started.duration_since(previous).as_secs_f64() * 1000.0);
        self.previous_started = Some(started);

        if self.samples.len() == MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample {
            timings,
            cadence_ms,
        });
    }

    pub fn snapshot_if_due(&mut self, force: bool) -> Option<PerformanceSnapshot> {
        if !force && self.last_publish.elapsed() < PUBLISH_INTERVAL {
            return None;
        }
        self.last_publish = Instant::now();
        Some(self.snapshot())
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
            frame_wait: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.frame_wait_ms)
            }),
            capture: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.capture_ms)
            }),
            preprocess: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.preprocess_ms)
            }),
            inference: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.inference_ms)
            }),
            postprocess: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.postprocess_ms)
            }),
            mak_write: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.mak_write_ms)
            }),
            total: summarize_samples(&self.samples, &mut scratch, |sample| {
                Some(sample.timings.total_ms)
            }),
            cold_run: self.cold_run,
        }
    }
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
    let summary = if len == 0 {
        MetricSummary::default()
    } else {
        let values = &mut scratch[..len];
        values.sort_unstable_by(f64::total_cmp);
        MetricSummary {
            p50_ms: percentile(values, 0.50),
            p95_ms: percentile(values, 0.95),
        }
    };
    summary
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_is_kept_as_cold_run() {
        let mut metrics = Metrics::new();
        let started = Instant::now();
        metrics.record(
            started,
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
        let base = Instant::now();
        metrics.record(base, StageTimings::default());
        for i in 1..=20 {
            metrics.record(
                base + Duration::from_millis(i * 10),
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
