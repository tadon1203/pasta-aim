//! One synchronous frame transaction; no acquisition starts until output returns.

use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;

use super::{Worker, run::ActiveRun};
use crate::{
    engine::{
        Command, RunMode,
        frame_trace::FrameTraceRecord,
        output::{CommandInfo, TargetInfo},
        target_track::Track,
    },
    graphics::{
        CapturePoll, GpuDropReason, GpuFaultReport, GpuFrameMetadata, NoDesktopUpdateReason,
    },
    inference::pipeline::InferenceResult,
    metrics::{CaptureSourceMetrics, StageTimings},
    settings::FpsLimit,
};

pub(super) struct FrameReport {
    pub generation: u64,
    pub metadata: GpuFrameMetadata,
    pub inference_start_qpc: i64,
    pub timings: StageTimings,
    pub detected: bool,
    pub target: Option<TargetInfo>,
    pub command: Option<CommandInfo>,
    pub track: Option<Track>,
    pub output_complete_qpc: i64,
    pub one_shot_done: bool,
}

#[derive(Default)]
pub(super) struct CaptureStats {
    fresh: u64,
    processed: u64,
    timeouts: u64,
    accumulated: u64,
}

pub(super) fn next_capture_deadline(start: Instant, fps: FpsLimit) -> Instant {
    start + Duration::from_secs_f64(1.0 / f64::from(fps.value()))
}

pub(super) fn capture_due(mode: RunMode, now: Instant, deadline: Instant) -> bool {
    mode == RunMode::OneShot || now >= deadline
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
}

struct InferredFrame {
    metadata: GpuFrameMetadata,
    result: InferenceResult,
    started_qpc: i64,
    ended_qpc: i64,
}

impl Worker {
    pub(super) fn step_frame(&mut self, commands: &Receiver<Command>) -> bool {
        if !self.drain_commands(commands) {
            return false;
        }
        let Some(run) = self.run.active else {
            return true;
        };
        self.next_capture = next_capture_deadline(Instant::now(), self.tuning.fps_limit);
        serial_frame(&mut FrameStep {
            worker: self,
            commands,
            run,
        })
    }

    pub(super) fn publish_capture_stats(&mut self, force: bool) {
        let elapsed = self.capture_stats_started.elapsed();
        if !force && elapsed < Duration::from_secs(1) {
            return;
        }
        let stats = std::mem::take(&mut self.capture_stats);
        let seconds = elapsed.as_secs_f64().max(f64::EPSILON);
        self.metrics.update_capture_source(CaptureSourceMetrics {
            fresh_fps: stats.fresh as f64 / seconds,
            processed_fps: stats.processed as f64 / seconds,
            timeouts: stats.timeouts,
            mean_accumulated_frames: if stats.fresh == 0 {
                0.0
            } else {
                stats.accumulated as f64 / stats.fresh as f64
            },
        });
        self.capture_stats_started = Instant::now();
        if let Some(performance) = self.metrics.snapshot_if_due(force) {
            self.status.edit(|status| status.performance = performance);
        }
    }

    fn acquire_frame(&mut self, run: ActiveRun) -> Option<GpuFrameMetadata> {
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
        let Some(capture) = self.capture.as_mut() else {
            self.fail_run("capture is not configured".to_owned());
            return None;
        };
        let frame = match capture.acquire(settings.wait_ms) {
            Ok(CapturePoll::Fresh(frame)) => {
                self.capture_stats.fresh += 1;
                self.capture_stats.accumulated += u64::from(frame.accumulated_frames());
                self.source_seq = self.source_seq.saturating_add(1);
                frame
            }
            Ok(CapturePoll::NoDesktopUpdate(reason)) => {
                if matches!(reason, NoDesktopUpdateReason::Timeout) {
                    self.capture_stats.timeouts += 1;
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
        while let Some(sample) = capture.try_take_gpu_copy_sample() {
            self.metrics.record_gpu_copy(sample.gpu_copy_ms);
        }
        Some(GpuFrameMetadata {
            pipeline_id: model.pipeline_id,
            generation: run.generation,
            frame_seq: self.frame_seq,
            source_seq: self.source_seq,
            present_qpc: submit.present_qpc,
            ready_qpc: self.qpc.now(),
            capture_ready_value: ready_value,
            accumulated_frames: submit.accumulated_frames,
            desktop_size: submit.desktop_size,
            acquire_wait_ms: submit.acquire_wait_ms,
            capture_ms: submit.capture_ms,
            copy_submit_ms: submit.copy_submit_ms,
        })
    }
}

impl SerialStages for FrameStep<'_> {
    type Frame = GpuFrameMetadata;
    type Inferred = InferredFrame;

    fn acquire(&mut self) -> Option<Self::Frame> {
        self.worker.acquire_frame(self.run)
    }

    fn infer(&mut self, metadata: Self::Frame) -> Option<Self::Inferred> {
        let worker = &mut self.worker;
        let model = worker
            .model
            .as_mut()
            .expect("acquisition requires a prepared model");
        let started_qpc = worker.qpc.now();
        match model.pipeline.infer(metadata) {
            Ok(result) => {
                model.last_completion = metadata.capture_ready_value;
                Some(InferredFrame {
                    metadata,
                    result,
                    started_qpc,
                    ended_qpc: worker.qpc.now(),
                })
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
        let InferredFrame {
            metadata,
            result,
            started_qpc,
            ended_qpc,
        } = frame;
        if !worker.run.is_active(metadata.generation)
            || worker.active_pipeline_id != Some(metadata.pipeline_id)
        {
            worker
                .metrics
                .record_gpu_drop(GpuDropReason::StaleGeneration);
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
        worker.capture_stats.processed += 1;
        let timings = StageTimings {
            present_to_gpu_submit_ms: qpc.elapsed_ms(metadata.present_qpc, metadata.ready_qpc),
            present_to_inference_start_ms: qpc.elapsed_ms(metadata.present_qpc, started_qpc),
            acquire_wait_ms: metadata.acquire_wait_ms,
            capture_ms: metadata.capture_ms,
            copy_submit_ms: metadata.copy_submit_ms,
            gpu_preprocess_ms: result.gpu_preprocess_ms,
            ort_run_wall_ms: result.ort_run_wall_ms,
            postprocess_ms: result.postprocess_ms,
            makcu_write_ms: qpc.elapsed_ms(output_start_qpc, output_complete_qpc),
            total_ms: qpc.elapsed_ms(metadata.present_qpc, output_complete_qpc),
        };
        if let Some(trace) = &worker.trace {
            trace.record(FrameTraceRecord {
                frame_seq: metadata.frame_seq,
                source_seq: metadata.source_seq,
                present_qpc: metadata.present_qpc,
                ready_qpc: metadata.ready_qpc,
                inference_start_qpc: started_qpc,
                output_start_qpc,
                output_complete_qpc,
                candidates: result.candidates,
                selected: result.detection,
                track: outcome.track,
                planned: outcome.planned,
                sent: outcome.command,
            });
        }
        if crate::logging::frame_trace_enabled() {
            tracing::trace!(
                pipeline_id = metadata.pipeline_id, generation = metadata.generation,
                frame_seq = metadata.frame_seq, source_seq = metadata.source_seq,
                accumulated_frames = metadata.accumulated_frames,
                acquire_wait_ms = timings.acquire_wait_ms, capture_ms = timings.capture_ms,
                gpu_preprocess_ms = ?timings.gpu_preprocess_ms,
                ort_run_wall_ms = timings.ort_run_wall_ms,
                inference_span_ms = qpc.elapsed_ms(started_qpc, ended_qpc),
                makcu_write_ms = timings.makcu_write_ms, total_ms = timings.total_ms,
                target = ?outcome.target, command = ?outcome.command,
                "frame pipeline completed"
            );
        }
        worker.handle_frame(FrameReport {
            generation: metadata.generation,
            metadata,
            inference_start_qpc: started_qpc,
            timings,
            detected: result.detection.is_some(),
            target: outcome.target,
            command: outcome.command,
            track: outcome.track,
            output_complete_qpc,
            one_shot_done: self.run.mode == RunMode::OneShot,
        });
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
            CapturePhase, EngineStatus, RunPhase,
            loop_metrics::LoopMetrics,
            output::{OutputSettings, OutputStage},
            telemetry::{RunSummary, StatusHub},
        },
        inference::{AdapterLuid, Candidates, Detection},
        makcu::MakcuClient,
        metrics::Metrics,
        timing::QpcClock,
    };
    use std::sync::{Arc, Mutex, mpsc};

    fn worker() -> (Worker, Arc<makxd::MockTransport>) {
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
        let (makcu, transport) = MakcuClient::mock();
        let ui_ctx = eframe::egui::Context::default();
        let (event_tx, _) = mpsc::channel();
        let mut worker = Worker {
            event_tx,
            status: StatusHub::new(
                Arc::new(Mutex::new(EngineStatus::default())),
                ui_ctx.clone(),
            ),
            ui_ctx,
            qpc: QpcClock::new().unwrap(),
            tuning,
            model: None,
            capture: None,
            output_stage: OutputStage::new(tuning.output),
            trace: None,
            next_capture: Instant::now(),
            frame_seq: 0,
            source_seq: 0,
            capture_stats: CaptureStats::default(),
            capture_stats_started: Instant::now(),
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
            summary: RunSummary::new(),
            metrics: Metrics::new(),
            loop_metrics: LoopMetrics::new(),
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
        (worker, transport)
    }

    fn inferred(worker: &Worker, present_qpc: i64) -> InferredFrame {
        InferredFrame {
            metadata: GpuFrameMetadata {
                pipeline_id: 1,
                generation: worker.run.active.unwrap().generation,
                frame_seq: 1,
                source_seq: 1,
                present_qpc,
                ready_qpc: present_qpc,
                capture_ready_value: 1,
                accumulated_frames: 1,
                desktop_size: (100, 100),
                acquire_wait_ms: 0.0,
                capture_ms: 0.0,
                copy_submit_ms: 0.0,
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
                ort_run_wall_ms: 0.0,
                postprocess_ms: 0.0,
                gpu_preprocess_ms: None,
            },
            started_qpc: present_qpc,
            ended_qpc: present_qpc,
        }
    }

    fn finish(worker: &mut Worker, commands: &Receiver<Command>, frame: InferredFrame) -> bool {
        let run = worker.run.active.unwrap();
        FrameStep {
            worker,
            commands,
            run,
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
            let (mut worker, transport) = worker();
            let frame = inferred(&worker, worker.qpc.now());
            let (tx, rx) = crossbeam_channel::unbounded();
            tx.send(command).unwrap();
            assert_eq!(finish(&mut worker, &rx, frame), !shutdown);
            assert!(transport.sent_commands().is_empty());
            assert!(worker.status.get().run.last_command.is_none());
        }
    }

    #[test]
    fn valid_result_outputs() {
        let (mut worker, transport) = worker();
        let (_tx, rx) = crossbeam_channel::unbounded();
        let frame = inferred(&worker, worker.qpc.now());
        assert!(finish(&mut worker, &rx, frame));
        assert!(!transport.sent_commands().is_empty());
    }

    #[test]
    fn failed_model_and_display_changes_preserve_selection_and_run_policy() {
        let (mut worker, _) = worker();
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
        let (mut worker, transport) = worker();
        worker.stop_run();
        worker.start_run(RunMode::OneShot);
        let now = Instant::now();
        assert!(capture_due(
            RunMode::OneShot,
            now,
            now + Duration::from_secs(1)
        ));
        assert!(!capture_due(
            RunMode::Continuous,
            now,
            now + Duration::from_secs(1)
        ));
        let (_tx, rx) = crossbeam_channel::unbounded();
        let frame = inferred(&worker, worker.qpc.now());
        assert!(finish(&mut worker, &rx, frame));
        assert!(!transport.sent_commands().is_empty());
        assert!(worker.run.active.is_none());
        assert_eq!(worker.status.get().run.phase, RunPhase::Idle);
    }

    #[test]
    fn stop_before_acquisition_and_shutdown_during_fps_wait_are_handled() {
        let (mut worker, _) = worker();
        let (tx, rx) = crossbeam_channel::unbounded();
        tx.send(Command::SetContinuous(false)).unwrap();
        assert!(worker.step_frame(&rx));
        // There is no GPU pipeline in this fixture: acquisition would fail the run.
        assert!(worker.status.get().run.error.is_none());
        worker.start_run(RunMode::Continuous);
        worker.next_capture = Instant::now() + Duration::from_secs(10);
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            tx.send(Command::Shutdown).unwrap();
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
        assert_eq!(
            next_capture_deadline(start, fps),
            start + Duration::from_millis(10)
        );
        let late = start + Duration::from_secs(1);
        assert_eq!(
            next_capture_deadline(late, fps),
            late + Duration::from_millis(10)
        );
    }
}
