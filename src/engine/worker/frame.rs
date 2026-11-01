//! One frame of the engine: the pipeline's attempt, the pending commands, and the output.

use std::{
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

use super::Worker;
use crate::{
    clock::Clock,
    engine::{Command, RunPhase},
    pipeline::{FrameOutcome, PipelineFault, ProcessedFrame},
    settings::FpsLimit,
};

/// The earliest time of the next capture in a Continuous run.
pub(super) struct CaptureDeadline {
    next: Instant,
}

impl CaptureDeadline {
    /// Due now.
    pub(super) fn new() -> Self {
        Self {
            next: Instant::now(),
        }
    }

    /// Called for a capture that got a new frame. `start` is the start of that capture attempt.
    pub(super) fn arm(&mut self, start: Instant, fps: FpsLimit) {
        self.next = start + Duration::from_secs_f64(1.0 / f64::from(fps.get()));
    }

    /// Called at the start of a run and when the FPS limit changes.
    pub(super) fn clear(&mut self) {
        self.next = Instant::now();
    }

    /// `None` when a capture is due now. Run once is always due.
    pub(super) fn pending_until(&self, mode: RunPhase, now: Instant) -> Option<Instant> {
        (mode == RunPhase::Continuous && now < self.next).then_some(self.next)
    }

    /// Only a step that got a new frame arms the deadline. Any other step leaves it as it is.
    pub(super) fn after_step(&mut self, outcome: StepOutcome, fps: FpsLimit) {
        if let StepOutcome::NewFrame { attempt_start } = outcome {
            self.arm(attempt_start, fps);
        }
    }
}

/// The result of one frame step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StepOutcome {
    /// The attempt got a new frame, also when a stop suppresses its output.
    NewFrame {
        attempt_start: Instant,
    },
    /// A timeout, a pointer-only update, an access loss, a fault, or no active run after the drain.
    NoDesktopUpdate,
    Shutdown,
}

impl Worker {
    pub(super) fn step_frame(&mut self, commands: &Receiver<Command>) -> StepOutcome {
        if !self.drain_commands(commands) {
            return StepOutcome::Shutdown;
        }
        if self.run.active.is_none() {
            return StepOutcome::NoDesktopUpdate;
        }
        let attempt_start = Instant::now();
        let wait_ms = u32::from(self.tuning.new_frame_wait.get());
        let result = if let Some(pipeline) = self.capture.pipeline_mut() {
            pipeline.process_next(wait_ms)
        } else {
            self.fail_run("inference pipeline is not prepared".to_owned());
            return StepOutcome::NoDesktopUpdate;
        };
        match result {
            Ok(FrameOutcome::NoDesktopUpdate) => StepOutcome::NoDesktopUpdate,
            Ok(FrameOutcome::NewFrame(frame)) => {
                if self.finish_frame(commands, attempt_start, &frame) {
                    StepOutcome::NewFrame { attempt_start }
                } else {
                    StepOutcome::Shutdown
                }
            }
            Err(fault) => {
                self.handle_fault(&fault);
                StepOutcome::NoDesktopUpdate
            }
        }
    }

    /// The part of a frame after the pipeline: the pending commands, the stop check, the output, and the
    /// statistics. Returns false on shutdown.
    fn finish_frame(
        &mut self,
        commands: &Receiver<Command>,
        attempt_start: Instant,
        frame: &ProcessedFrame,
    ) -> bool {
        if !self.drain_commands(commands) {
            return false;
        }
        // The frame is current when its run is still active and the run began before the capture.
        let Some(run) = self.run.active.filter(|run| run.started <= attempt_start) else {
            self.recorder.frame_dropped();
            return true;
        };
        let output_start = Clock::now();
        let outcome = self.aim.update(frame.selected, self.input_size);
        if let Some(rounded) = outcome.rounded
            && let Err(error) = self.output.write(rounded)
        {
            self.output_lost(format!("{error:#}"));
            return true;
        }
        let output_done = Clock::now();
        self.recorder
            .frame(frame, &outcome, output_start, output_done, Instant::now());
        self.note_output(frame.desktop_size);
        if run.mode == RunPhase::OneShot {
            self.stop_run();
        }
        true
    }

    /// Suspends the pipeline after a fault. The user retries with `RetryCapture`.
    fn handle_fault(&mut self, fault: &PipelineFault) {
        self.recorder.frame_dropped();
        self.suspend_invalid_pipeline(&fault.message);
    }

    /// Edits the state for a frame that reached the output. Each edit is made only when its value changes.
    fn note_output(&mut self, desktop_size: (usize, usize)) {
        if self.state.capture.desktop_size != Some(desktop_size) {
            self.edit(|state| state.capture.desktop_size = Some(desktop_size));
        }
        if self.state.run.error.is_some() {
            self.edit(|state| state.run.error = None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Source, Tuning, run::RunControl};
    use super::*;
    use crate::{
        aim::{Aim, AimSettings},
        channel::channel,
        clock::{self, Clock, Wake},
        engine::{CapturePhase, OutputPhase, RunPhase, State, stats::Recorder},
        output::{Output, fake::FakeMakcu},
        pipeline::{AdapterLuid, Detection, FrameTimes},
        settings::{Interpolation, InterpolationPercent, Settings},
    };
    use std::sync::mpsc;

    /// The Move frames that the fake MAKCU received.
    fn moves(fake: &FakeMakcu) -> Vec<Vec<u8>> {
        fake.frames()
            .into_iter()
            .filter(|frame| frame[4] == 0x18)
            .collect()
    }

    fn worker() -> (Worker, FakeMakcu) {
        let (worker, fake, _waker) = worker_with_waker();
        (worker, fake)
    }

    /// The fixture, with the waker of its wait, which the test raises to wake the loop.
    /// It has no GPU, so it begins the run without the readiness check.
    fn worker_with_waker() -> (Worker, FakeMakcu, Wake) {
        let settings = Settings::default();
        let tuning = Tuning {
            aim: AimSettings {
                sensitivity: settings.sensitivity,
                target_y_offset: settings.target_y_offset,
                max_delta: settings.max_delta,
            },
            interpolation: settings.interpolation,
            confidence: settings.confidence,
            fps_limit: settings.fps_limit,
            new_frame_wait: settings.new_frame_wait,
            low_latency: settings.low_latency,
        };
        let (output, fake) = Output::fake();
        let (ui, _messages) = channel(|| {});
        let (wait, waker) = clock::wait().unwrap();
        let clock = Clock::new().unwrap();
        let mut worker = Worker {
            ui,
            state: State::default(),
            state_changed: false,
            recorder: Recorder::new(clock),
            stats_sent: Instant::now(),
            clock,
            tuning,
            displays: Vec::new(),
            // The fixture has no GPU capture and no pipeline.
            capture: Source::None,
            input_size: (100, 100),
            wait,
            capture_deadline: CaptureDeadline::new(),
            active_model: Some(("previous.onnx".into(), "previous.onnx".into())),
            next_pipeline_id: 2,
            capture_adapter: 0,
            capture_output: 0,
            selected_adapter_luid: AdapterLuid { low: 0, high: 0 },
            aim: Aim::new(tuning.aim),
            output,
            run: RunControl::default(),
            com_ready: false,
        };
        worker.edit(|state| {
            state.capture.phase = CapturePhase::Ready;
            state.capture.selected = Some(crate::pipeline::DisplayInfo {
                adapter_index: 0,
                output_index: 0,
                adapter_luid: AdapterLuid { low: 0, high: 0 },
                adapter_name: "test GPU".into(),
                display_name: "test display".into(),
                width: 100,
                height: 100,
                refresh_hz: 60.0,
            });
        });
        worker.begin_run(RunPhase::Continuous);
        (worker, fake, waker)
    }

    /// A new frame with a valid detection, as the pipeline would hand it to the engine.
    fn processed(present_qpc: i64) -> ProcessedFrame {
        ProcessedFrame {
            desktop_size: (100, 100),
            times: FrameTimes {
                present: present_qpc,
                attempt_start: present_qpc,
                acquired: present_qpc,
                copy_submitted: present_qpc,
                ready: present_qpc,
                inference_start: present_qpc,
                inference_done: present_qpc,
                postprocess_done: present_qpc,
            },
            gpu_preprocess_ms: None,
            gpu_copy_ms: None,
            selected: Some(Detection::for_test(70.0, 50.0, 0)),
        }
    }

    /// Finishes a frame whose capture was attempted now.
    fn finish(worker: &mut Worker, commands: &Receiver<Command>, frame: &ProcessedFrame) -> bool {
        worker.finish_frame(commands, Instant::now(), frame)
    }

    #[test]
    fn queued_stop_shutdown_and_pipeline_edits_suppress_old_output() {
        for command in [
            Command::SetContinuous(false),
            Command::Shutdown,
            Command::LoadModel {
                name: "missing.onnx".into(),
                path: "missing.onnx".into(),
                notify: false,
            },
            Command::SetCaptureSelection {
                adapter: 99,
                output: 99,
            },
            Command::SetLowLatency(true),
        ] {
            let shutdown = matches!(command, Command::Shutdown);
            let (mut worker, fake) = worker();
            let frame = processed(Clock::now());
            let (tx, rx) = mpsc::channel();
            tx.send(command).unwrap();
            assert_eq!(finish(&mut worker, &rx, &frame), !shutdown);
            assert_eq!(moves(&fake).len(), 0);
            // A stale frame is counted as dropped. A shutdown ends before the output stage.
            assert_eq!(worker.recorder.counts(), (0, u64::from(!shutdown)));
        }
    }

    #[test]
    fn valid_result_outputs() {
        let (mut worker, fake) = worker();
        let (_tx, rx) = mpsc::channel();
        let frame = processed(Clock::now());
        assert!(finish(&mut worker, &rx, &frame));
        assert_ne!(moves(&fake).len(), 0);
        assert_eq!(worker.recorder.counts(), (1, 0));
        let stats = worker.recorder.stats();
        assert_eq!(
            stats.last_target.map(|target| (target.0, target.1)),
            Some((20.0, 0.0))
        );
        assert!(stats.last_move.is_some());
    }

    #[test]
    fn a_failed_move_write_is_a_lost_makcu_and_stops_the_run() {
        let (mut worker, fake) = worker();
        fake.disconnect();
        let (_tx, rx) = mpsc::channel();
        assert!(finish(&mut worker, &rx, &processed(Clock::now())));
        let state = worker.state;
        assert_eq!(state.output.phase, OutputPhase::Failed);
        assert!(state.output.error.is_some());
        assert!(worker.run.active.is_none());
        assert!(!worker.output.is_connected());
        assert_eq!(
            worker.recorder.counts().0,
            0,
            "the frame did not reach the output"
        );
    }

    #[test]
    fn the_stored_interpolation_is_sent_and_a_missing_reply_is_a_lost_makcu() {
        let (mut worker, fake) = worker();
        worker.tuning.interpolation = Interpolation::Percent(InterpolationPercent::new(40));
        worker.send_interpolation();
        assert_eq!(fake.frames().last().unwrap()[4..], [0x1F, 40]);
        assert!(worker.output.is_connected());

        fake.mute_next(0x1F);
        worker.send_interpolation();
        assert_eq!(worker.state.output.phase, OutputPhase::Failed);
        assert!(!worker.output.is_connected());
    }

    #[test]
    fn a_rejected_interpolation_shows_an_error_and_keeps_the_makcu() {
        let (mut worker, fake) = worker();
        worker.tuning.interpolation = Interpolation::Percent(InterpolationPercent::new(40));
        fake.reject_next(0x1F);
        worker.send_interpolation();
        assert!(worker.state.run.error.is_some());
        assert!(worker.output.is_connected());
    }

    #[test]
    fn failed_model_and_display_changes_preserve_selection_and_run_policy() {
        let (mut worker, _fake) = worker();
        worker.set_capture_selection(99, 99);
        assert_eq!(worker.capture_output, 0);
        assert!(worker.run.is_continuous());
        worker.load_model(false, "missing.onnx".into(), "missing.onnx".into());
        assert_eq!(worker.active_model.as_ref().unwrap().0, "previous.onnx");
        assert!(worker.run.active.is_none());
    }

    #[test]
    fn one_shot_ignores_fps_wait_and_finishes_after_one_output() {
        let (mut worker, fake) = worker();
        worker.stop_run();
        worker.begin_run(RunPhase::OneShot);
        let now = Instant::now();
        let deadline = CaptureDeadline {
            next: now + Duration::from_secs(1),
        };
        assert_eq!(deadline.pending_until(RunPhase::OneShot, now), None);
        assert_eq!(
            deadline.pending_until(RunPhase::Continuous, now),
            Some(now + Duration::from_secs(1))
        );
        let (_tx, rx) = mpsc::channel();
        let frame = processed(Clock::now());
        assert!(finish(&mut worker, &rx, &frame));
        assert_ne!(moves(&fake).len(), 0);
        assert!(worker.run.active.is_none());
        assert_eq!(worker.state.run.phase, RunPhase::Idle);
        assert_eq!(worker.recorder.counts().0, 1);
    }

    #[test]
    fn a_frame_captured_before_a_new_run_is_dropped() {
        let (mut worker, _fake) = worker();
        let attempt_start = Instant::now();
        worker.stop_run();
        worker.begin_run(RunPhase::Continuous);
        let (_tx, rx) = mpsc::channel();
        // The run that is active now began after the capture: the frame is stale.
        assert!(worker.finish_frame(&rx, attempt_start, &processed(Clock::now())));
        assert_eq!(worker.recorder.counts(), (0, 1));
    }

    #[test]
    fn stop_before_acquisition_and_shutdown_during_fps_wait_are_handled() {
        let (mut worker, _, waker) = worker_with_waker();
        let (tx, rx) = mpsc::channel();
        tx.send(Command::SetContinuous(false)).unwrap();
        assert_eq!(worker.step_frame(&rx), StepOutcome::NoDesktopUpdate);
        // There is no GPU pipeline in this fixture: acquisition would fail the run.
        assert!(worker.state.run.error.is_none());
        worker.begin_run(RunPhase::Continuous);
        worker.capture_deadline.next = Instant::now() + Duration::from_secs(10);
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            tx.send(Command::Shutdown).unwrap();
            waker.wake();
        });
        let start = Instant::now();
        worker.event_loop(&rx);
        sender.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(worker.state.run.error.is_none());
    }

    #[test]
    fn fps_deadline_does_not_accumulate_idle_credit() {
        let start = Instant::now();
        let fps = FpsLimit::new(100);
        let mut deadline = CaptureDeadline::new();
        deadline.arm(start, fps);
        assert_eq!(deadline.next, start + Duration::from_millis(10));
        let late = start + Duration::from_secs(1);
        deadline.arm(late, fps);
        assert_eq!(deadline.next, late + Duration::from_millis(10));
    }

    #[test]
    fn a_step_without_a_new_frame_does_not_arm_the_deadline() {
        let fps = FpsLimit::new(100);
        let start = Instant::now();
        let mut deadline = CaptureDeadline::new();
        let before = deadline.next;
        deadline.after_step(StepOutcome::NoDesktopUpdate, fps);
        assert_eq!(deadline.next, before);
        deadline.after_step(
            StepOutcome::NewFrame {
                attempt_start: start,
            },
            fps,
        );
        assert_eq!(
            deadline.pending_until(RunPhase::Continuous, start),
            Some(start + Duration::from_millis(10))
        );
    }
}
