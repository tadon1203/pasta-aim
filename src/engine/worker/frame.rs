//! One synchronous frame transaction; no acquisition starts until output returns.

use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;

use super::{Worker, run::ActiveRun};
use crate::{
    engine::{Command, RunMode},
    graphics::{
        CapturePoll, GpuDropReason, GpuFaultReport, GpuFrameMetadata, NoDesktopUpdateReason,
    },
    inference::pipeline::InferenceResult,
    settings::FpsLimit,
    telemetry::{FrameRecord, FrameTimes},
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
    /// The attempt got a New frame, also when a stop suppresses its output or a fault ends the Run.
    NewFrame {
        attempt_start: Instant,
    },
    /// A timeout, a pointer-only update, an access loss, or no active Run after the drain.
    NoDesktopUpdate,
    Shutdown,
}

/// This ordering is shared with the hardware-free pipeline tests below.
trait SerialStages {
    type Frame;
    type Inferred;
    fn acquire(&mut self) -> Option<Self::Frame>;
    fn infer(&mut self, frame: Self::Frame) -> Option<Self::Inferred>;
    /// False means shutdown, including a command received during inference.
    fn output(&mut self, inferred: Self::Inferred) -> bool;
}

fn serial_frame(stages: &mut impl SerialStages) -> bool {
    let Some(frame) = stages.acquire() else {
        return true;
    };
    let Some(inferred) = stages.infer(frame) else {
        return true;
    };
    stages.output(inferred)
}

struct FrameStep<'a> {
    worker: &'a mut Worker,
    commands: &'a Receiver<Command>,
    run: ActiveRun,
    attempt_start: Instant,
    new_frame: bool,
}

struct InferredFrame {
    /// The stage boundaries are complete: the inference and the postprocess are filled in.
    metadata: GpuFrameMetadata,
    result: InferenceResult,
}

impl Worker {
    pub(super) fn step_frame(&mut self, commands: &Receiver<Command>) -> StepOutcome {
        if !self.drain_commands(commands) {
            return StepOutcome::Shutdown;
        }
        let Some(run) = self.run.active else {
            return StepOutcome::NoDesktopUpdate;
        };
        let mut step = FrameStep {
            worker: self,
            commands,
            run,
            attempt_start: Instant::now(),
            new_frame: false,
        };
        if !serial_frame(&mut step) {
            return StepOutcome::Shutdown;
        }
        if step.new_frame {
            StepOutcome::NewFrame {
                attempt_start: step.attempt_start,
            }
        } else {
            StepOutcome::NoDesktopUpdate
        }
    }

    /// `new_frame` is set when the capture got a New frame, even if a later step of this attempt fails.
    fn acquire_frame(&mut self, run: ActiveRun, new_frame: &mut bool) -> Option<GpuFrameMetadata> {
        let Some(model) = self.model.as_ref() else {
            self.fail_run("inference pipeline is not prepared".to_owned());
            return None;
        };
        if let Err(error) = model.pipeline.ensure_frame_reusable(model.last_completion) {
            let device_lost = model.pipeline.device_lost();
            self.recover_poisoned_pipeline(
                format!("GPU frame reuse failed: {error:#}"),
                device_lost,
                None,
            );
            return None;
        }
        let size = model.input_size;
        let settings = self.tuning.capture;
        let qpc = self.qpc;
        let Some(capture) = self.capture.as_mut() else {
            self.fail_run("capture is not configured".to_owned());
            return None;
        };
        let frame = match capture.acquire(settings.wait_ms, qpc) {
            Ok(CapturePoll::Fresh(frame)) => {
                *new_frame = true;
                self.run_counters.new_frames += 1;
                self.run_counters.accumulated_frames += u64::from(frame.accumulated_frames());
                frame
            }
            Ok(CapturePoll::NoDesktopUpdate(reason)) => {
                if matches!(reason, NoDesktopUpdateReason::Timeout) {
                    self.run_counters.timeouts += 1;
                }
                return None;
            }
            Err(error) => {
                let device_lost = capture.device_lost();
                if device_lost {
                    self.recover_poisoned_pipeline(
                        format!("DXGI capture failed: {error:#}"),
                        true,
                        None,
                    );
                } else {
                    self.suspend_invalid_pipeline(format!("DXGI capture failed: {error:#}"));
                }
                return None;
            }
        };
        self.frame_seq = self
            .frame_seq
            .checked_add(1)
            .expect("frame sequence exhausted");
        let model = self.model.as_mut().expect("model checked above");
        let ready_value = self.frame_seq;
        let submit = capture.copy_center_to(
            &frame,
            &model.capture_frame.texture,
            &model.capture_frame.capture_ready_fence,
            ready_value,
            size,
            qpc,
        );
        let submit = match submit {
            Ok(submit) => submit,
            Err(error) => {
                let device_lost = capture.device_lost() || model.pipeline.device_lost();
                let report = GpuFaultReport {
                    pipeline_id: model.pipeline_id,
                    prepare_id: model.prepare_id.clone(),
                    generation: Some(run.generation),
                    stage: capture.last_fault_stage(),
                    frame_seq: Some(self.frame_seq),
                    ready_value: Some(ready_value),
                    ready_completed: capture.last_ready_completed(),
                    reuse_value: Some(model.last_completion),
                    reuse_completed: None,
                    hr: error
                        .downcast_ref::<windows::core::Error>()
                        .map(|e| e.code().0),
                    d3d11_removed_reason: capture.device_removed_reason(),
                    d3d12_removed_reason: None,
                    error: format!("{error:#}"),
                    dred: None,
                };
                self.recover_poisoned_pipeline(
                    format!("GPU capture copy failed: {error:#}"),
                    device_lost,
                    Some(report),
                );
                return None;
            }
        };
        // One GPU copy time per New frame. A sample that is not ready yet waits for the next frame.
        let gpu_copy_ms = capture
            .try_take_gpu_copy_sample()
            .map(|sample| sample.gpu_copy_ms);
        Some(GpuFrameMetadata {
            pipeline_id: model.pipeline_id,
            generation: run.generation,
            frame_seq: self.frame_seq,
            times: FrameTimes {
                present: submit.present_qpc,
                attempt_start: submit.attempt_start_qpc,
                acquired: submit.acquired_qpc,
                copy_submitted: submit.copy_submitted_qpc,
                ready: submit.ready_qpc,
                inference_start: 0,
                inference_done: 0,
                postprocess_done: 0,
            },
            capture_ready_value: ready_value,
            desktop_size: submit.desktop_size,
            gpu_copy_ms,
        })
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

impl SerialStages for FrameStep<'_> {
    type Frame = GpuFrameMetadata;
    type Inferred = InferredFrame;

    fn acquire(&mut self) -> Option<Self::Frame> {
        self.worker.acquire_frame(self.run, &mut self.new_frame)
    }

    fn infer(&mut self, mut metadata: Self::Frame) -> Option<Self::Inferred> {
        let worker = &mut self.worker;
        let qpc = worker.qpc;
        let model = worker
            .model
            .as_mut()
            .expect("acquisition requires a prepared model");
        metadata.times.inference_start = qpc.now();
        match model.pipeline.infer(metadata, qpc) {
            Ok(result) => {
                model.last_completion = metadata.capture_ready_value;
                metadata.times.inference_done = result.inference_done_qpc;
                metadata.times.postprocess_done = result.postprocess_done_qpc;
                Some(InferredFrame { metadata, result })
            }
            Err(error) => {
                let report = model.pipeline.take_fault_report();
                let device_lost = model.pipeline.device_lost();
                if report.is_some() || device_lost {
                    worker.recover_poisoned_pipeline(
                        format!("GPU pipeline failed: {error:#}"),
                        device_lost,
                        report,
                    );
                } else {
                    worker.suspend_invalid_pipeline(format!("inference failed: {error:#}"));
                }
                None
            }
        }
    }

    fn output(&mut self, frame: Self::Inferred) -> bool {
        let worker = &mut self.worker;
        if !worker.drain_commands(self.commands) {
            return false;
        }
        let InferredFrame { metadata, result } = frame;
        if !worker.run.is_active(metadata.generation)
            || worker.active_pipeline_id != Some(metadata.pipeline_id)
        {
            worker
                .telemetry
                .frame_dropped(GpuDropReason::StaleGeneration);
            return true;
        }
        let qpc = worker.qpc;
        let output_start_qpc = qpc.now();
        let outcome = match worker
            .output_stage
            .emit(&mut worker.makcu, result.detection)
        {
            Ok(outcome) => outcome,
            Err(error) => {
                worker.fail_run(format!("Inference output failed: {error:#}"));
                return true;
            }
        };
        let output_complete_qpc = qpc.now();
        worker.telemetry.frame(FrameRecord {
            generation: metadata.generation,
            frame_seq: metadata.frame_seq,
            times: metadata.times,
            output_start_qpc,
            output_complete_qpc,
            gpu_preprocess_ms: result.gpu_preprocess_ms,
            gpu_copy_ms: metadata.gpu_copy_ms,
            candidates: result.candidates,
            selected: result.detection,
            track: outcome.track,
            aim_error: outcome.target.map(|(dx, dy, _, _)| (dx, dy)),
            planned: outcome.planned,
            sent: outcome.command,
        });
        worker.note_output(metadata.desktop_size);
        if self.run.mode == RunMode::OneShot {
            worker.stop_run();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Tuning, run::RunControl};
    use super::*;
    use crate::{
        config::Config,
        engine::{
            CapturePhase, EngineState, RunPhase,
            output::{OutputSettings, OutputStage},
            status::StatusHub,
        },
        inference::{AdapterLuid, Candidates, Detection},
        makcu::{FakeMakcu, MakcuClient, RawStats},
        telemetry::{RunCounters, TelemetryMessage, TelemetrySink},
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
            model: None,
            capture: None,
            // The fixture has no GPU capture, so it stands in for a ready one.
            capture_ready: true,
            output_stage: OutputStage::new(tuning.output),
            wait,
            capture_deadline: CaptureDeadline::new(),
            frame_seq: 0,
            run_counters: RunCounters::default(),
            sent_counters: RunCounters::default(),
            sent_raw: RawStats::default(),
            active_model: Some(("previous.onnx".into(), "previous.onnx".into())),
            model_ready: true,
            auto_reprepare_attempted: false,
            next_pipeline_id: 2,
            active_pipeline_id: Some(1),
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
        worker.start_run(RunMode::Continuous);
        (worker, fake, waker, telemetry_rx)
    }

    fn inferred(worker: &Worker, present_qpc: i64) -> InferredFrame {
        InferredFrame {
            metadata: GpuFrameMetadata {
                pipeline_id: 1,
                generation: worker.run.active.unwrap().generation,
                frame_seq: 1,
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
                capture_ready_value: 1,
                desktop_size: (100, 100),
                gpu_copy_ms: None,
            },
            result: InferenceResult {
                detection: Some(Detection {
                    center_x: 70.0,
                    center_y: 50.0,
                    box_width: 10.0,
                    box_height: 10.0,
                    confidence: 0.9,
                    class_id: 0,
                }),
                candidates: Candidates::EMPTY,
                inference_done_qpc: present_qpc,
                postprocess_done_qpc: present_qpc,
                gpu_preprocess_ms: None,
            },
        }
    }

    fn finish(worker: &mut Worker, commands: &Receiver<Command>, frame: InferredFrame) -> bool {
        let run = worker.run.active.unwrap();
        FrameStep {
            worker,
            commands,
            run,
            attempt_start: Instant::now(),
            new_frame: true,
        }
        .output(frame)
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
            let frame = inferred(&worker, worker.qpc.now());
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
        let frame = inferred(&worker, worker.qpc.now());
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
        assert_eq!(worker.active_pipeline_id, Some(1));
        assert_eq!(worker.capture_output, 0);
        assert!(worker.run.is_continuous());
        assert_ne!(worker.run.active.unwrap().generation, old_generation);
        worker.load_model(false, "missing.onnx".into(), "missing.onnx".into());
        assert_eq!(worker.active_model.as_ref().unwrap().0, "previous.onnx");
        assert_eq!(worker.active_pipeline_id, Some(1));
        assert!(worker.model_ready);
        assert!(worker.run.active.is_none());
    }

    #[test]
    fn one_shot_ignores_fps_wait_and_finishes_after_one_output() {
        let (mut worker, fake, telemetry) = worker();
        worker.stop_run();
        worker.start_run(RunMode::OneShot);
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
        let frame = inferred(&worker, worker.qpc.now());
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
        worker.start_run(RunMode::Continuous);
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

    #[derive(Default)]
    struct FakeStages {
        events: Vec<&'static str>,
        no_image: bool,
        infer_fails: bool,
    }

    impl SerialStages for FakeStages {
        type Frame = ();
        type Inferred = ();
        fn acquire(&mut self) -> Option<()> {
            self.events.push("acquire");
            (!self.no_image).then_some(())
        }
        fn infer(&mut self, (): ()) -> Option<()> {
            self.events.extend(["preprocess", "infer"]);
            (!self.infer_fails).then_some(())
        }
        fn output(&mut self, (): ()) -> bool {
            self.events.extend(["output", "output complete"]);
            true
        }
    }

    #[test]
    fn next_acquisition_follows_completed_output() {
        let mut stages = FakeStages::default();
        assert!(serial_frame(&mut stages));
        assert!(serial_frame(&mut stages));
        assert_eq!(
            stages.events,
            [
                "acquire",
                "preprocess",
                "infer",
                "output",
                "output complete",
                "acquire",
                "preprocess",
                "infer",
                "output",
                "output complete"
            ]
        );
    }

    #[test]
    fn absent_image_and_inference_failure_do_not_output() {
        let mut stages = FakeStages {
            no_image: true,
            ..Default::default()
        };
        serial_frame(&mut stages);
        assert_eq!(stages.events, ["acquire"]);
        stages.events.clear();
        stages.no_image = false;
        stages.infer_fails = true;
        serial_frame(&mut stages);
        assert_eq!(stages.events, ["acquire", "preprocess", "infer"]);
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
