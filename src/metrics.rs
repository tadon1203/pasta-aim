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
        let cadence: Vec<f64> = self.samples.iter().filter_map(|s| s.cadence_ms).collect();
        let mean_cadence = if cadence.is_empty() {
            None
        } else {
            Some(cadence.iter().sum::<f64>() / cadence.len() as f64)
        };

        PerformanceSnapshot {
            sample_count: self.samples.len(),
            fps: mean_cadence
                .filter(|value| *value > 0.0)
                .map(|value| 1000.0 / value)
                .unwrap_or(0.0),
            cadence: summarize(&cadence),
            frame_wait: summarize_stage(&self.samples, |s| s.frame_wait_ms),
            capture: summarize_stage(&self.samples, |s| s.capture_ms),
            preprocess: summarize_stage(&self.samples, |s| s.preprocess_ms),
            inference: summarize_stage(&self.samples, |s| s.inference_ms),
            postprocess: summarize_stage(&self.samples, |s| s.postprocess_ms),
            mak_write: summarize_stage(&self.samples, |s| s.mak_write_ms),
            total: summarize_stage(&self.samples, |s| s.total_ms),
            cold_run: self.cold_run,
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

fn summarize_stage(
    samples: &VecDeque<Sample>,
    select: impl Fn(StageTimings) -> f64,
) -> MetricSummary {
    let values: Vec<f64> = samples
        .iter()
        .map(|sample| select(sample.timings))
        .collect();
    summarize(&values)
}

fn summarize(values: &[f64]) -> MetricSummary {
    if values.is_empty() {
        return MetricSummary::default();
    }

    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    MetricSummary {
        p50_ms: percentile(&sorted, 0.50),
        p95_ms: percentile(&sorted, 0.95),
    }
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
        assert!(snapshot.total.p50_ms >= 10.0 && snapshot.total.p50_ms <= 11.0);
        assert!(snapshot.total.p95_ms >= 19.0);
        assert!(snapshot.fps > 90.0 && snapshot.fps < 110.0);
    }
}
