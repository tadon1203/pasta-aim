//! One frame of the engine: the Pipeline's attempt, the pending commands, and the output.

use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;

use super::{Worker, run::ActiveRun};
use crate::{
    engine::{Command, RunGeneration, RunMode},
    graphics::{GpuDropReason, GpuFaultReport, NoDesktopUpdateReason},
    pipeline::{FaultKind, FrameOutcome, PipelineFault, ProcessedFrame},
    settings::FpsLimit,
    telemetry::FrameRecord,
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

    /// Called for a capture that got a New frame. `start` is the start of that capture attempt.
    pub(super) fn arm(&mut self, start: Instant, fps: FpsLimit) {
        self.next = start + Duration::from_secs_f64(1.0 / f64::from(fps.value()));
    }

    /// Called at the start of a Run and when the FPS limit changes.
    pub(super) fn clear(&mut self) {
        self.next = Instant::now();
    }

    /// `None` when a capture is due now. Run once is always due.
    pub(super) fn pending_until(&self, mode: RunMode, now: Instant) -> Option<Instant> {
        (mode == RunMode::Continuous && now < self.next).then_some(self.next)
    }

    /// Only a step that got a New frame arms the deadline. Any other step leaves it as it is.
    pub(super) fn after_step(&mut self, outcome: StepOutcome, fps: FpsLimit) {
        if let StepOutcome::NewFrame { attempt_start } = outcome {
            self.arm(attempt_start, fps);
        }
    }
}

/// The result of one frame step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StepOutcome {
    /// The attempt got a New frame, also when a stop suppresses its output.
    NewFrame {
        attempt_start: Instant,
    },
    /// A timeout, a pointer-only update, an access loss, a fault, or no active Run after the drain.
    NoDesktopUpdate,
    Shutdown,
}

impl Worker {
    pub(super) fn step_frame(&mut self, commands: &Receiver<Command>) -> StepOutcome {
        if !self.drain_commands(commands) {
            return StepOutcome::Shutdown;
        }
        let Some(run) = self.run.active else {
            return StepOutcome::NoDesktopUpdate;
        };
        let attempt_start = Instant::now();
        let wait_ms = self.tuning.capture.wait_ms;
        let result = match self.capture.pipeline_mut() {
            Some(pipeline) => pipeline.process_next(wait_ms),
            None => {
                self.fail_run("inference pipeline is not prepared".to_owned());
                return StepOutcome::NoDesktopUpdate;
            }
        };
        match result {
            Ok(FrameOutcome::NoDesktopUpdate(reason)) => {
                if matches!(reason, NoDesktopUpdateReason::Timeout) {
                    self.run_counters.timeouts += 1;
                }
                StepOutcome::NoDesktopUpdate
            }
            Ok(FrameOutcome::NewFrame(frame)) => {
                self.run_counters.new_frames += 1;
                self.run_counters.accumulated_frames += u64::from(frame.accumulated_frames);
                if self.finish_frame(commands, run, frame) {
                    StepOutcome::NewFrame { attempt_start }
                } else {
                    StepOutcome::Shutdown
                }
            }
            Err(fault) => {
                self.handle_fault(fault, run.generation);
                StepOutcome::NoDesktopUpdate
            }
        }
    }

    /// The part of a frame after the Pipeline: the pending commands, the stop check, the output, and the
    /// Frame record. Returns false on shutdown.
    fn finish_frame(
        &mut self,
        commands: &Receiver<Command>,
        run: ActiveRun,
        frame: ProcessedFrame,
    ) -> bool {
        if !self.drain_commands(commands) {
            return false;
        }
        if !self.run.is_active(run.generation) {
            self.telemetry.frame_dropped(GpuDropReason::StaleGeneration);
            return true;
        }
        let output_start_qpc = self.qpc.now();
        let outcome = match self.output_stage.emit(&mut self.makcu, frame.selected) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.fail_run(format!("Inference output failed: {error:#}"));
                return true;
            }
        };
        let output_complete_qpc = self.qpc.now();
        self.telemetry.frame(FrameRecord {
            generation: run.generation,
            frame_seq: frame.frame_seq,
            times: frame.times,
            output_start_qpc,
            output_complete_qpc,
            gpu_preprocess_ms: frame.gpu_preprocess_ms,
            gpu_copy_ms: frame.gpu_copy_ms,
            candidates: frame.candidates,
            selected: frame.selected,
            track: outcome.track,
            aim_error: outcome.target.map(|(dx, dy, _, _)| (dx, dy)),
            planned: outcome.planned,
            sent: outcome.command,
        });
        self.note_output(frame.desktop_size);
        if run.mode == RunMode::OneShot {
            self.stop_run();
        }
        true
    }

    /// Recovers from a Pipeline fault. The engine adds the Run generation to the report.
    fn handle_fault(&mut self, fault: PipelineFault, generation: RunGeneration) {
        let PipelineFault {
            kind,
            message,
            report,
        } = fault;
        let report = report.map(|report| GpuFaultReport {
            generation: Some(generation),
            ..*report
        });
        match kind {
            FaultKind::Poisoned { device_lost } => {
                self.recover_poisoned_pipeline(message, device_lost, report);
            }
            FaultKind::Invalid => self.suspend_invalid_pipeline(message),
        }
    }

    /// Edits the state for a frame that reached the output. Each edit is made only when its value changes.
    fn note_output(&mut self, desktop_size: (usize, usize)) {
        if self.status.get().capture.desktop_size != Some(desktop_size) {
            self.status
                .edit(|status| status.capture.desktop_size = Some(desktop_size));
        }
        if self.status.get().run.error.is_some() {
            self.status.edit(|status| status.run.error = None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{CaptureState, Tuning, run::RunControl};
    use super::*;
    use crate::{
        config::Config,
        engine::{
            CapturePhase, EngineState, RunPhase,
            output::{OutputSettings, OutputStage},
            status::StatusHub,
        },
        inference::{AdapterLuid, Candidates, Detection},
        makcu::{FakeMakcu, MakcuClient},
        telemetry::{FrameTimes, RunCounters, TelemetryMessage, TelemetrySink},
        timing::QpcClock,
        wait::{DeadlineWait, Waker},
    };
    use crossbeam_channel::Receiver as TestReceiver;
    use std::sync::{Arc, Mutex, mpsc};

    /// The Move frames that the fake MAKCU received.
    fn moves(fake: &FakeMakcu) -> Vec<Vec<u8>> {
        fake.frames()
            .into_iter()
            .filter(|frame| frame[4] == 0x18)
            .collect()
    }

    /// The Frame records and the dropped frames that the telemetry queue received.
    fn output_messages(telemetry: &TestReceiver<TelemetryMessage>) -> (Vec<FrameRecord>, usize) {
        let mut records = Vec::new();
        let mut dropped = 0;
        for message in telemetry.try_iter() {
            match message {
                TelemetryMessage::Frame(record) => records.push(record),
                TelemetryMessage::FrameDropped(_) => dropped += 1,
                _ => {}
            }
        }
        (records, dropped)
    }

    fn worker() -> (Worker, FakeMakcu, TestReceiver<TelemetryMessage>) {
        let (worker, fake, _waker, telemetry) = worker_with_waker();
        (worker, fake, telemetry)
    }

    /// The fixture, with the waker of its wait, which the test raises to wake the loop, and the telemetry queue.
    /// It has no GPU, so it begins the Run without the readiness check.
    fn worker_with_waker() -> (Worker, FakeMakcu, Waker, TestReceiver<TelemetryMessage>) {
        let config = Config::default();
        let tuning = Tuning {
            output: OutputSettings {
                sensitivity: config.sensitivity,
                target_y_offset: config.target_y_offset,
                max_delta: config.max_delta,
            },
            confidence_threshold: config.confidence_threshold,
            fps_limit: config.fps_limit,
            capture: config.capture,
            low_latency: config.low_latency,
            frame_trace: false,
        };
        let (makcu, fake) = MakcuClient::fake();
        let ui_ctx = eframe::egui::Context::default();
        let (event_tx, _) = mpsc::channel();
        let (wait, waker) = DeadlineWait::new().unwrap();
        let (telemetry, telemetry_rx) = TelemetrySink::for_test();
        let mut worker = Worker {
            event_tx,
            status: StatusHub::new(
                Arc::new(Mutex::new(Arc::new(EngineState::default()))),
                ui_ctx.clone(),
            ),
            ui_ctx,
            qpc: QpcClock::new().unwrap(),
            telemetry,
            tuning,
            displays: Vec::new(),
            // The fixture has no GPU capture and no Pipeline.
            capture: CaptureState::None,
            output_stage: OutputStage::new(tuning.output),
            wait,
            capture_deadline: CaptureDeadline::new(),
            run_counters: RunCounters::default(),
            sent_counters: RunCounters::default(),
            active_model: Some(("previous.onnx".into(), "previous.onnx".into())),
            auto_reprepare_attempted: false,
            next_pipeline_id: 2,
            capture_adapter: 0,
            capture_output: 0,
            selected_adapter_luid: AdapterLuid { low: 0, high: 0 },
            makcu,
            run: RunControl::default(),
            graphics: None,
        };
        worker.status.edit(|status| {
            status.capture.phase = CapturePhase::Ready;
            status.capture.selected = Some(crate::graphics::DisplayInfo {
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
        worker.output_stage.set_input_size((100, 100));
        worker.begin_run(RunMode::Continuous);
        (worker, fake, waker, telemetry_rx)
    }

    /// A New frame with a valid detection, as the Pipeline would hand it to the engine.
    fn processed(present_qpc: i64) -> ProcessedFrame {
        ProcessedFrame {
            frame_seq: 1,
            accumulated_frames: 1,
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
            selected: Some(Detection {
                center_x: 70.0,
                center_y: 50.0,
                box_width: 10.0,
                box_height: 10.0,
                confidence: 0.9,
                class_id: 0,
            }),
            candidates: Candidates::EMPTY,
        }
    }

    fn finish(worker: &mut Worker, commands: &Receiver<Command>, frame: ProcessedFrame) -> bool {
        let run = worker.run.active.unwrap();
        worker.finish_frame(commands, run, frame)
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
            let (mut worker, fake, telemetry) = worker();
            let frame = processed(worker.qpc.now());
            let (tx, rx) = crossbeam_channel::unbounded();
            tx.send(command).unwrap();
            assert_eq!(finish(&mut worker, &rx, frame), !shutdown);
            assert!(moves(&fake).is_empty());
            let (records, dropped) = output_messages(&telemetry);
            assert!(records.is_empty());
            // A stale frame is reported as dropped. A shutdown ends before the output stage.
            assert_eq!(dropped, usize::from(!shutdown));
        }
    }

    #[test]
    fn valid_result_outputs() {
        let (mut worker, fake, telemetry) = worker();
        let (_tx, rx) = crossbeam_channel::unbounded();
        let frame = processed(worker.qpc.now());
        assert!(finish(&mut worker, &rx, frame));
        assert!(!moves(&fake).is_empty());
        let (records, dropped) = output_messages(&telemetry);
        assert_eq!(dropped, 0);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].frame_seq, 1);
        assert_eq!(records[0].aim_error, Some((20.0, 0.0)));
        assert!(records[0].sent.is_some());
    }

    #[test]
    fn failed_model_and_display_changes_preserve_selection_and_run_policy() {
        let (mut worker, _fake, _telemetry) = worker();
        let old_generation = worker.run.active.unwrap().generation;
        worker.set_capture_selection(99, 99);
        assert_eq!(worker.capture_output, 0);
        assert!(worker.run.is_continuous());
        assert_ne!(worker.run.active.unwrap().generation, old_generation);
        worker.load_model(false, "missing.onnx".into(), "missing.onnx".into());
        assert_eq!(worker.active_model.as_ref().unwrap().0, "previous.onnx");
        assert!(worker.run.active.is_none());
    }

    #[test]
    fn one_shot_ignores_fps_wait_and_finishes_after_one_output() {
        let (mut worker, fake, telemetry) = worker();
        worker.stop_run();
        worker.begin_run(RunMode::OneShot);
        let now = Instant::now();
        let deadline = CaptureDeadline {
            next: now + Duration::from_secs(1),
        };
        assert_eq!(deadline.pending_until(RunMode::OneShot, now), None);
        assert_eq!(
            deadline.pending_until(RunMode::Continuous, now),
            Some(now + Duration::from_secs(1))
        );
        let (_tx, rx) = crossbeam_channel::unbounded();
        let frame = processed(worker.qpc.now());
        assert!(finish(&mut worker, &rx, frame));
        assert!(!moves(&fake).is_empty());
        assert!(worker.run.active.is_none());
        assert_eq!(worker.status.get().run.phase, RunPhase::Idle);
        let (records, _) = output_messages(&telemetry);
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn stop_before_acquisition_and_shutdown_during_fps_wait_are_handled() {
        let (mut worker, _, waker, _telemetry) = worker_with_waker();
        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(Command::SetContinuous(false)).unwrap();
        assert_eq!(worker.step_frame(&rx), StepOutcome::NoDesktopUpdate);
        // There is no GPU pipeline in this fixture: acquisition would fail the run.
        assert!(worker.status.get().run.error.is_none());
        worker.begin_run(RunMode::Continuous);
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
        assert!(worker.status.get().run.error.is_none());
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
            deadline.pending_until(RunMode::Continuous, start),
            Some(start + Duration::from_millis(10))
        );
    }
}
