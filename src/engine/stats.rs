use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use crate::{
    aim::{Move, Outcome},
    clock::Clock,
    pipeline::Frame,
};

pub(super) const PUBLISH_INTERVAL: Duration = Duration::from_millis(100);
const RATE_WINDOW: Duration = Duration::from_secs(1);
const MAX_SAMPLES: usize = 256;

#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub last_target: Option<(f32, f32, f32, i32)>,
    pub last_move: Option<Move>,
    pub last_total_ms: Option<f64>,
    pub stages: Stages,
    pub rates: Rates,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Rates {
    pub fresh_fps: f64,
    pub processed_fps: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Stages {
    pub sample_count: usize,
    pub fps: f64,
    pub cadence: Percentiles,
    pub present_to_gpu_submit: Percentiles,
    pub present_to_inference_start: Percentiles,
    pub acquire_wait: Percentiles,
    pub capture: Percentiles,
    pub copy_submit: Percentiles,
    pub gpu_copy: Percentiles,
    pub preprocess: Percentiles,
    pub inference: Percentiles,
    pub postprocess: Percentiles,
    pub output_write: Percentiles,
    pub total: Percentiles,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Percentiles {
    pub p50: f64,
    pub p95: f64,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    present_to_gpu_submit: f64,
    present_to_inference_start: f64,
    acquire_wait: f64,
    capture: f64,
    copy_submit: f64,
    preprocess: Option<f64>,
    inference: f64,
    postprocess: f64,
    output_write: f64,
    total: f64,
    cadence: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct RateWindow {
    started: Instant,
    frames: u64,
    dropped: u64,
}

pub(super) struct Recorder {
    clock: Clock,
    samples: VecDeque<Sample>,
    gpu_copy: VecDeque<f64>,
    skip_next: bool,
    previous_started: Option<i64>,
    frames: u64,
    dropped: u64,
    window: RateWindow,
    rates: Rates,
    last_target: Option<(f32, f32, f32, i32)>,
    last_move: Option<Move>,
    last_total_ms: Option<f64>,
    changed: bool,
}

impl Recorder {
    pub(super) fn new(clock: Clock) -> Self {
        Self {
            clock,
            samples: VecDeque::with_capacity(MAX_SAMPLES),
            gpu_copy: VecDeque::with_capacity(MAX_SAMPLES),
            skip_next: true,
            previous_started: None,
            frames: 0,
            dropped: 0,
            window: RateWindow {
                started: Instant::now(),
                frames: 0,
                dropped: 0,
            },
            rates: Rates::default(),
            last_target: None,
            last_move: None,
            last_total_ms: None,
            changed: false,
        }
    }

    pub(super) fn run_started(&mut self, now: Instant) {
        let clock = self.clock;
        *self = Self {
            skip_next: self.skip_next,
            window: RateWindow {
                started: now,
                frames: 0,
                dropped: 0,
            },
            changed: true,
            ..Self::new(clock)
        };
    }

    pub(super) fn pipeline_changed(&mut self) {
        self.skip_next = true;
        self.samples.clear();
        self.gpu_copy.clear();
        self.previous_started = None;
        self.rates = Rates::default();
        self.changed = true;
    }

    pub(super) fn frame_dropped(&mut self) {
        self.dropped += 1;
    }

    #[cfg(test)]
    pub(super) fn counts(&self) -> (u64, u64) {
        (self.frames, self.dropped)
    }

    pub(super) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    pub(super) fn frame(
        &mut self,
        frame: &Frame,
        outcome: &Outcome,
        output_start: i64,
        output_done: i64,
        now: Instant,
    ) {
        let times = frame.times;
        let clock = self.clock;
        let total = clock.ms(times.present, output_done);
        self.frames += 1;
        self.changed = true;
        self.last_target = outcome
            .error
            .zip(frame.selected)
            .map(|((dx, dy), detection)| (dx, dy, detection.confidence, detection.class_id));
        self.last_move = outcome.rounded;
        self.last_total_ms = Some(total);

        if self.skip_next {
            self.skip_next = false;
            self.previous_started = None;
        } else {
            let cadence = self
                .previous_started
                .map(|previous| clock.ms(previous, times.inference_start));
            self.previous_started = Some(times.inference_start);
            push_bounded(
                &mut self.samples,
                Sample {
                    present_to_gpu_submit: clock.ms(times.present, times.ready),
                    present_to_inference_start: clock.ms(times.present, times.inference_start),
                    acquire_wait: clock.ms(times.attempt_start, times.acquired),
                    capture: clock.ms(times.acquired, times.ready),
                    copy_submit: clock.ms(times.acquired, times.copy_submitted),
                    preprocess: frame.gpu_preprocess_ms,
                    inference: clock.ms(times.inference_start, times.inference_done),
                    postprocess: clock.ms(times.inference_done, times.postprocess_done),
                    output_write: clock.ms(output_start, output_done),
                    total,
                    cadence,
                },
            );
            if let Some(gpu_copy_ms) = frame.gpu_copy_ms {
                push_bounded(&mut self.gpu_copy, gpu_copy_ms);
            }
        }
        self.update_rates(now);
    }

    pub(super) fn stats(&self) -> Stats {
        Stats {
            last_target: self.last_target,
            last_move: self.last_move,
            last_total_ms: self.last_total_ms,
            stages: self.stages(),
            rates: self.rates,
        }
    }

    fn update_rates(&mut self, now: Instant) {
        let elapsed = now.duration_since(self.window.started);
        if elapsed < RATE_WINDOW {
            return;
        }
        let seconds = elapsed.as_secs_f64();
        let processed = self.frames - self.window.frames;
        let dropped = self.dropped - self.window.dropped;
        self.rates = Rates {
            fresh_fps: (processed + dropped) as f64 / seconds,
            processed_fps: processed as f64 / seconds,
        };
        self.window = RateWindow {
            started: now,
            frames: self.frames,
            dropped: self.dropped,
        };
    }

    fn stages(&self) -> Stages {
        let samples = &self.samples;
        let (cadence_sum, cadence_count) = samples
            .iter()
            .filter_map(|sample| sample.cadence)
            .fold((0.0, 0_u32), |(sum, count), value| (sum + value, count + 1));
        let mean_cadence = (cadence_count > 0).then(|| cadence_sum / f64::from(cadence_count));
        let of = |select: fn(&Sample) -> Option<f64>| {
            percentiles(samples.iter().filter_map(select).collect())
        };
        Stages {
            sample_count: samples.len(),
            fps: mean_cadence
                .filter(|value| *value > 0.0)
                .map_or(0.0, |value| 1000.0 / value),
            cadence: of(|sample| sample.cadence),
            present_to_gpu_submit: of(|sample| Some(sample.present_to_gpu_submit)),
            present_to_inference_start: of(|sample| Some(sample.present_to_inference_start)),
            acquire_wait: of(|sample| Some(sample.acquire_wait)),
            capture: of(|sample| Some(sample.capture)),
            copy_submit: of(|sample| Some(sample.copy_submit)),
            gpu_copy: percentiles(self.gpu_copy.iter().copied().collect()),
            preprocess: of(|sample| sample.preprocess),
            inference: of(|sample| Some(sample.inference)),
            postprocess: of(|sample| Some(sample.postprocess)),
            output_write: of(|sample| Some(sample.output_write)),
            total: of(|sample| Some(sample.total)),
        }
    }
}

fn percentiles(mut values: Vec<f64>) -> Percentiles {
    Percentiles {
        p50: percentile(&mut values, 50),
        p95: percentile(&mut values, 95),
    }
}

fn percentile(values: &mut [f64], percent: usize) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let index = ((values.len() - 1) * percent + 50) / 100;
    *values.select_nth_unstable_by(index, f64::total_cmp).1
}

fn push_bounded<T>(queue: &mut VecDeque<T>, value: T) {
    if queue.len() == MAX_SAMPLES {
        queue.pop_front();
    }
    queue.push_back(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::Times;

    const QPC: Clock = Clock::test(1_000.0);

    fn frame(present: i64) -> Frame {
        Frame {
            desktop_size: (100, 100),
            times: Times {
                present,
                attempt_start: present + 10,
                acquired: present + 12,
                copy_submitted: present + 13,
                ready: present + 16,
                inference_start: present + 17,
                inference_done: present + 22,
                postprocess_done: present + 24,
            },
            gpu_preprocess_ms: None,
            gpu_copy_ms: None,
            selected: None,
        }
    }

    fn outcome() -> Outcome {
        Outcome {
            rounded: Some(Move { x: 2, y: -2 }),
            ..Outcome::default()
        }
    }

    fn record(recorder: &mut Recorder, present: i64, total: i64, now: Instant) {
        recorder.frame(
            &frame(present),
            &outcome(),
            present + total - 2,
            present + total,
            now,
        );
    }

    #[test]
    fn the_first_frame_of_a_pipeline_is_not_a_sample() {
        let mut recorder = Recorder::new(QPC);
        let now = Instant::now();
        record(&mut recorder, 0, 30, now);
        record(&mut recorder, 100, 40, now);
        assert_eq!(recorder.stats().stages.sample_count, 1);

        recorder.pipeline_changed();
        assert_eq!(recorder.stats().stages.sample_count, 0);
        record(&mut recorder, 200, 50, now);
        assert_eq!(recorder.stats().stages.sample_count, 0);
        record(&mut recorder, 300, 60, now);
        assert_eq!(recorder.stats().stages.total.p50, 60.0);
    }

    #[test]
    fn percentiles_use_rolling_samples() {
        let mut recorder = Recorder::new(QPC);
        let now = Instant::now();
        record(&mut recorder, 0, 0, now);
        for i in 1..=20 {
            record(&mut recorder, i * 10, i, now);
        }
        let stages = recorder.stats().stages;
        assert_eq!(stages.sample_count, 20);
        assert_eq!(stages.total.p50, 11.0);
        assert_eq!(stages.total.p95, 19.0);
        assert_eq!(stages.cadence.p50, 10.0);
        assert_eq!(stages.cadence.p95, 10.0);
        assert_eq!(stages.fps, 100.0);
    }

    #[test]
    fn rates_come_from_frames_one_window_apart() {
        let mut recorder = Recorder::new(QPC);
        let start = Instant::now();
        recorder.run_started(start);
        record(&mut recorder, 0, 30, start);
        record(&mut recorder, 100, 30, start + Duration::from_millis(500));
        assert_eq!(recorder.stats().rates.fresh_fps, 0.0);

        recorder.frame_dropped();
        record(&mut recorder, 200, 30, start + Duration::from_secs(1));
        let rates = recorder.stats().rates;
        assert_eq!(rates.processed_fps, 3.0);
        assert_eq!(rates.fresh_fps, 4.0);
    }

    #[test]
    fn a_new_run_starts_the_totals_again() {
        let mut recorder = Recorder::new(QPC);
        let now = Instant::now();
        record(&mut recorder, 0, 30, now);
        record(&mut recorder, 100, 30, now);
        recorder.run_started(now);
        let stats = recorder.stats();
        assert_eq!(stats.stages.sample_count, 0);
        assert!(stats.last_move.is_none());
    }

    #[test]
    fn changes_are_taken_once() {
        let mut recorder = Recorder::new(QPC);
        assert!(!recorder.take_changed());
        record(&mut recorder, 0, 30, Instant::now());
        assert!(recorder.take_changed());
        assert!(!recorder.take_changed());
    }
}
