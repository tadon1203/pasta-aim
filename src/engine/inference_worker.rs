use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::Instant,
};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};
use windows::Win32::Graphics::Direct3D11::ID3D11Device4;

use super::{
    RunGeneration, RunMode,
    output::{
        CommandInfo, MakSnapshot, MotionGate, OutputOutcome, OutputSettings, OutputStage,
        TargetInfo,
    },
};
use crate::{
    graphics::{
        CaptureFramePool, FrameConsumer, FramePublisher, GpuDropReason, GpuFaultReport,
        GpuFrameMetadata,
    },
    inference::{
        AdapterLuid, EpAssignmentState, TensorPrecision,
        pipeline::{InferencePipeline, PreparedPipeline, prepare_directml_pipeline},
    },
    mak::MakClient,
    metrics::StageTimings,
    settings::ConfidenceThreshold,
    timing::QpcClock,
};

pub(super) struct PreparedModelInfo {
    pub pipeline_id: u64,
    pub prepare_id: String,
    pub input_size: (usize, usize),
    pub precision: TensorPrecision,
    pub ep_assignment: Arc<EpAssignmentState>,
    pub capture_frames: CaptureFramePool,
}

/// Outcome of one processed frame, sent to the engine thread for telemetry.
pub(super) struct FrameReport {
    pub generation: RunGeneration,
    pub metadata: GpuFrameMetadata,
    pub inference_start_qpc: i64,
    pub timings: StageTimings,
    pub detected: bool,
    pub target: Option<TargetInfo>,
    pub command: Option<CommandInfo>,
    pub mak: MakSnapshot,
    /// A one-shot run finished with this frame.
    pub one_shot_done: bool,
}

pub(super) enum InferenceEvent {
    Frame(Box<FrameReport>),
    Failed {
        generation: RunGeneration,
        error: String,
    },
    Poisoned {
        generation: RunGeneration,
        error: String,
        device_lost: bool,
        report: Option<Box<GpuFaultReport>>,
    },
    OutputFailed {
        generation: RunGeneration,
        error: String,
    },
}

/// Everything the inference thread needs besides its GPU pipeline.
pub(super) struct InferenceConfig {
    pub confidence_threshold: ConfidenceThreshold,
    pub output: OutputSettings,
    pub mak: Arc<Mutex<MakClient>>,
    pub gate: MotionGate,
}

enum InferenceCommand {
    LoadModel {
        path: PathBuf,
        adapter_luid: AdapterLuid,
        capture_device: ID3D11Device4,
        publisher: FramePublisher,
        pipeline_id: u64,
        reply: Sender<Result<PreparedModelInfo, String>>,
    },
    SetConfidenceThreshold(ConfidenceThreshold),
    SetOutput(OutputSettings),
    Start {
        generation: RunGeneration,
        mode: RunMode,
    },
    Stop {
        generation: RunGeneration,
        reply: Sender<()>,
    },
    Shutdown,
}

pub(super) struct InferenceWorker {
    control_tx: Sender<InferenceCommand>,
    event_rx: Receiver<InferenceEvent>,
    join: Option<JoinHandle<()>>,
}

impl InferenceWorker {
    pub(super) fn spawn(
        qpc: QpcClock,
        consumer: FrameConsumer,
        config: InferenceConfig,
    ) -> Result<Self> {
        let (control_tx, control_rx) = unbounded();
        let (event_tx, event_rx) = unbounded();
        let join = thread::Builder::new()
            .name("nice-aimassist-inference".to_owned())
            .spawn(move || {
                InferenceThread::new(control_rx, event_tx, consumer, qpc, config).run()
            })?;
        Ok(Self {
            control_tx,
            event_rx,
            join: Some(join),
        })
    }

    pub(super) fn load_model(
        &self,
        path: PathBuf,
        adapter_luid: AdapterLuid,
        capture_device: ID3D11Device4,
        publisher: FramePublisher,
        pipeline_id: u64,
    ) -> Result<PreparedModelInfo> {
        let (reply, response) = bounded(1);
        self.control_tx.send(InferenceCommand::LoadModel {
            path,
            adapter_luid,
            capture_device,
            publisher,
            pipeline_id,
            reply,
        })?;
        response.recv()?.map_err(anyhow::Error::msg)
    }

    pub(super) fn start(&self, generation: RunGeneration, mode: RunMode) -> Result<()> {
        self.control_tx
            .send(InferenceCommand::Start { generation, mode })?;
        Ok(())
    }

    pub(super) fn set_confidence_threshold(&self, value: ConfidenceThreshold) {
        let _ = self
            .control_tx
            .send(InferenceCommand::SetConfidenceThreshold(value));
    }

    pub(super) fn set_output(&self, settings: OutputSettings) {
        let _ = self.control_tx.send(InferenceCommand::SetOutput(settings));
    }

    /// Returns once the thread has stopped producing output for `generation`.
    pub(super) fn stop(&self, generation: RunGeneration) -> Result<()> {
        let (reply, response) = bounded(1);
        self.control_tx
            .send(InferenceCommand::Stop { generation, reply })?;
        response.recv()?;
        Ok(())
    }

    pub(super) fn event_rx(&self) -> &Receiver<InferenceEvent> {
        &self.event_rx
    }

    pub(super) fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = self.control_tx.send(InferenceCommand::Shutdown);
            let _ = join.join();
        }
    }
}

impl Drop for InferenceWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunState {
    Idle,
    Running {
        generation: RunGeneration,
        mode: RunMode,
    },
}

struct InferenceThread {
    /// First field, so the GPU pipeline is dropped before everything else.
    pipeline: Option<InferencePipeline>,
    control_rx: Receiver<InferenceCommand>,
    event_tx: Sender<InferenceEvent>,
    consumer: FrameConsumer,
    qpc: QpcClock,
    state: RunState,
    confidence_threshold: ConfidenceThreshold,
    output: OutputStage,
    gate: MotionGate,
}

impl InferenceThread {
    fn new(
        control_rx: Receiver<InferenceCommand>,
        event_tx: Sender<InferenceEvent>,
        consumer: FrameConsumer,
        qpc: QpcClock,
        config: InferenceConfig,
    ) -> Self {
        Self {
            control_rx,
            event_tx,
            consumer,
            qpc,
            pipeline: None,
            state: RunState::Idle,
            confidence_threshold: config.confidence_threshold,
            output: OutputStage::new(config.mak, config.output),
            gate: config.gate,
        }
    }

    fn run(mut self) {
        loop {
            let RunState::Running { generation, mode } = self.state else {
                match self.control_rx.recv() {
                    Ok(command) => {
                        if !self.handle_command(command) {
                            return;
                        }
                        continue;
                    }
                    Err(_) => return,
                }
            };
            while let Ok(command) = self.control_rx.try_recv() {
                if !self.handle_command(command) {
                    return;
                }
            }
            if self.state == (RunState::Running { generation, mode })
                && !self.step(generation, mode)
            {
                return;
            }
        }
    }

    /// Handles at most one frame. Returns false when the thread should exit.
    fn step(&mut self, generation: RunGeneration, mode: RunMode) -> bool {
        if self.pipeline.is_none() {
            self.fail(generation, "inference pipeline is not prepared".to_owned());
            return true;
        }
        let Some(mut frame) = self.consumer.try_take_latest() else {
            select! {
                recv(self.control_rx) -> command => {
                    if let Ok(command) = command {
                        return self.handle_command(command);
                    }
                    return false;
                },
                recv(self.consumer.ready_rx()) -> _ => {},
            }
            return true;
        };
        let metadata = *frame.metadata();
        if metadata.generation != generation {
            self.consumer.record_drop(GpuDropReason::StaleGeneration);
            return true;
        }
        // Presented before the last output finished, so it cannot show that motion.
        if self.gate.reject(metadata.present_qpc) {
            return true;
        }
        let pipeline = self.pipeline.as_mut().expect("checked above");
        let inference_start_qpc = self.qpc.now();
        let started = Instant::now();
        let result = pipeline.infer(&mut frame);
        let report = result
            .is_err()
            .then(|| pipeline.take_fault_report())
            .flatten();
        let device_lost = pipeline.device_lost();
        let inference_end_qpc = self.qpc.now();
        let poisoned = self.consumer.is_poisoned();
        drop(frame);
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let event = if poisoned {
                    InferenceEvent::Poisoned {
                        generation,
                        error: format!("GPU pipeline failed: {error:#}"),
                        device_lost,
                        report: report.map(Box::new),
                    }
                } else {
                    InferenceEvent::Failed {
                        generation,
                        error: format!("inference failed: {error:#}"),
                    }
                };
                let _ = self.event_tx.send(event);
                self.state = RunState::Idle;
                return true;
            }
        };
        tracing::trace!(
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            "GPU frame inference finished"
        );

        let one_shot = mode == RunMode::OneShot;
        let output_started_qpc = self.qpc.now();
        let outcome = match self.output.emit(result.detection, one_shot) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.output_failed(generation, error);
                return true;
            }
        };
        let output_complete_qpc = self.qpc.now();
        if outcome.command.is_some() {
            self.gate.mark_complete(output_complete_qpc);
        }
        let qpc = self.qpc;
        let timings = StageTimings {
            present_to_gpu_submit_ms: qpc.elapsed_ms(metadata.present_qpc, metadata.ready_qpc),
            present_to_inference_start_ms: qpc
                .elapsed_ms(metadata.present_qpc, inference_start_qpc),
            acquire_wait_ms: metadata.acquire_wait_ms,
            capture_ms: metadata.capture_ms,
            copy_submit_ms: metadata.copy_submit_ms,
            gpu_preprocess_ms: result.gpu_preprocess_ms,
            queue_age_ms: qpc.elapsed_ms(metadata.ready_qpc, inference_start_qpc),
            ort_run_wall_ms: result.ort_run_wall_ms,
            postprocess_ms: result.postprocess_ms,
            mak_write_ms: qpc.elapsed_ms(output_started_qpc, output_complete_qpc),
            total_ms: qpc.elapsed_ms(metadata.present_qpc, output_complete_qpc),
        };
        if crate::logging::frame_trace_enabled() {
            trace_frame(
                &metadata,
                &timings,
                qpc.elapsed_ms(inference_start_qpc, inference_end_qpc),
                &outcome,
            );
        }
        if outcome.pulse
            && let Err(error) = self.output.pulse_release()
        {
            self.output_failed(generation, error);
            return true;
        }
        if one_shot {
            self.state = RunState::Idle;
            self.consumer.clear();
        }
        let report = FrameReport {
            generation,
            metadata,
            inference_start_qpc,
            timings,
            detected: result.detection.is_some(),
            target: outcome.target,
            command: outcome.command,
            mak: self.output.mak_snapshot(),
            one_shot_done: one_shot,
        };
        let _ = self.event_tx.send(InferenceEvent::Frame(Box::new(report)));
        true
    }

    fn fail(&mut self, generation: RunGeneration, error: String) {
        let _ = self
            .event_tx
            .send(InferenceEvent::Failed { generation, error });
        self.state = RunState::Idle;
    }

    fn output_failed(&mut self, generation: RunGeneration, error: anyhow::Error) {
        let _ = self.event_tx.send(InferenceEvent::OutputFailed {
            generation,
            error: format!("{error:#}"),
        });
        self.state = RunState::Idle;
    }

    /// Returns false when the thread should exit.
    fn handle_command(&mut self, command: InferenceCommand) -> bool {
        match command {
            InferenceCommand::LoadModel {
                path,
                adapter_luid,
                capture_device,
                publisher,
                pipeline_id,
                reply,
            } => {
                let result =
                    self.load_model(&path, adapter_luid, &capture_device, publisher, pipeline_id);
                let _ = reply.send(result.map_err(|error| format!("{error:#}")));
            }
            InferenceCommand::SetConfidenceThreshold(value) => {
                self.confidence_threshold = value;
                if let Some(pipeline) = self.pipeline.as_mut() {
                    pipeline.set_confidence_threshold(value);
                }
            }
            InferenceCommand::SetOutput(settings) => self.output.set_settings(settings),
            InferenceCommand::Start { generation, mode } => {
                self.state = RunState::Running { generation, mode };
            }
            InferenceCommand::Stop { generation, reply } => {
                if matches!(self.state, RunState::Running { generation: active, .. } if active == generation)
                {
                    self.state = RunState::Idle;
                    self.consumer.clear();
                }
                let _ = reply.send(());
            }
            InferenceCommand::Shutdown => {
                self.state = RunState::Idle;
                return false;
            }
        }
        true
    }

    fn load_model(
        &mut self,
        path: &std::path::Path,
        adapter_luid: AdapterLuid,
        capture_device: &ID3D11Device4,
        publisher: FramePublisher,
        pipeline_id: u64,
    ) -> Result<PreparedModelInfo> {
        let PreparedPipeline {
            pipeline,
            capture_frames,
            ep_assignment,
            input_spec,
            prepare_id,
        } = prepare_directml_pipeline(
            path,
            adapter_luid,
            capture_device,
            publisher,
            pipeline_id,
            self.confidence_threshold,
        )?;
        let input_size = (input_spec.width, input_spec.height);
        self.output.set_input_size(input_size);
        self.pipeline = Some(pipeline);
        Ok(PreparedModelInfo {
            pipeline_id,
            prepare_id,
            input_size,
            precision: input_spec.precision,
            ep_assignment: Arc::new(ep_assignment),
            capture_frames,
        })
    }
}

fn trace_frame(
    metadata: &GpuFrameMetadata,
    timings: &StageTimings,
    inference_span_ms: f64,
    outcome: &OutputOutcome,
) {
    tracing::trace!(
        pipeline_id = metadata.pipeline_id,
        generation = metadata.generation,
        frame_seq = metadata.frame_seq,
        slot_id = metadata.slot_id,
        acquire_wait_ms = timings.acquire_wait_ms,
        capture_ms = timings.capture_ms,
        gpu_preprocess_ms = timings.gpu_preprocess_ms,
        queue_age_ms = timings.queue_age_ms,
        present_qpc = metadata.present_qpc,
        accumulated_frames = metadata.accumulated_frames,
        ort_run_wall_ms = timings.ort_run_wall_ms,
        inference_span_ms,
        mak_write_ms = timings.mak_write_ms,
        total_ms = timings.total_ms,
        target = ?outcome.target,
        command = ?outcome.command,
        "frame pipeline completed"
    );
}
