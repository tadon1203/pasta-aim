use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};
use windows::Win32::Graphics::Direct3D11::ID3D11Device4;

use super::RunGeneration;
use crate::{
    graphics::{
        CaptureFramePool, FrameConsumer, FramePublisher, GpuDropReason, GpuFaultReport,
        GpuFrameMetadata,
    },
    inference::{AdapterLuid, BackendInfo, pipeline::prepare_directml_pipeline},
    settings::ConfidenceThreshold,
    timing::QpcClock,
};

pub(super) struct PreparedModelInfo {
    pub pipeline_id: u64,
    pub prepare_id: String,
    pub input_size: (usize, usize),
    pub backend_info: BackendInfo,
    pub ep_assignment: crate::inference::EpAssignmentState,
    pub capture_frames: CaptureFramePool,
}

pub(super) struct InferenceFrameResult {
    pub metadata: GpuFrameMetadata,
    pub inference_start_qpc: i64,
    pub inference_end_qpc: i64,
    pub detection: Option<crate::inference::Detection>,
    pub ort_run_wall_ms: f64,
    pub postprocess_ms: f64,
    pub gpu_preprocess_ms: f64,
}

pub(super) enum InferenceControlEvent {
    Failed {
        generation: RunGeneration,
        error: String,
    },
    Poisoned {
        generation: RunGeneration,
        error: String,
        device_lost: bool,
        report: Option<GpuFaultReport>,
    },
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
    Start {
        generation: RunGeneration,
    },
    Stop {
        generation: RunGeneration,
        reply: Sender<()>,
    },
    Shutdown,
}

enum InferenceRunState {
    Idle,
    Running { generation: RunGeneration },
}

pub(super) struct InferenceWorker {
    control_tx: Sender<InferenceCommand>,
    latest_result: Arc<Mutex<Option<InferenceFrameResult>>>,
    result_ready_rx: Receiver<()>,
    event_rx: Receiver<InferenceControlEvent>,
    result_superseded: Arc<AtomicU64>,
    join: Option<JoinHandle<()>>,
}

impl InferenceWorker {
    pub(super) fn spawn(
        qpc: QpcClock,
        consumer: FrameConsumer,
        confidence_threshold: ConfidenceThreshold,
    ) -> Result<Self> {
        let (control_tx, control_rx) = unbounded();
        let (result_ready_tx, result_ready_rx) = bounded(1);
        let (event_tx, event_rx) = unbounded();
        let latest_result = Arc::new(Mutex::new(None));
        let result_superseded = Arc::new(AtomicU64::new(0));
        let thread_result = Arc::clone(&latest_result);
        let thread_superseded = Arc::clone(&result_superseded);
        let join = thread::Builder::new()
            .name("nice-aimassist-inference".to_owned())
            .spawn(move || {
                inference_loop(
                    control_rx,
                    event_tx,
                    result_ready_tx,
                    thread_result,
                    thread_superseded,
                    consumer,
                    qpc,
                    confidence_threshold,
                )
            })?;
        Ok(Self {
            control_tx,
            latest_result,
            result_ready_rx,
            event_rx,
            result_superseded,
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
        let (tx, rx) = bounded(1);
        self.control_tx.send(InferenceCommand::LoadModel {
            path,
            adapter_luid,
            capture_device,
            publisher,
            pipeline_id,
            reply: tx,
        })?;
        rx.recv()?.map_err(anyhow::Error::msg)
    }

    pub(super) fn start(&self, generation: RunGeneration) -> Result<()> {
        self.control_tx
            .send(InferenceCommand::Start { generation })?;
        Ok(())
    }
    pub(super) fn set_confidence_threshold(&self, value: ConfidenceThreshold) {
        let _ = self
            .control_tx
            .send(InferenceCommand::SetConfidenceThreshold(value));
    }
    pub(super) fn stop(&self, generation: RunGeneration) -> Result<()> {
        let (reply, response) = bounded(1);
        self.control_tx
            .send(InferenceCommand::Stop { generation, reply })?;
        response.recv()?;
        Ok(())
    }
    pub(super) fn result_ready_rx(&self) -> &Receiver<()> {
        &self.result_ready_rx
    }
    pub(super) fn take_latest_result(&self) -> Option<InferenceFrameResult> {
        self.latest_result
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }
    pub(super) fn event_rx(&self) -> &Receiver<InferenceControlEvent> {
        &self.event_rx
    }
    pub(super) fn take_result_superseded(&self) -> u64 {
        self.result_superseded.swap(0, Ordering::AcqRel)
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

#[allow(clippy::too_many_arguments)]
fn inference_loop(
    control_rx: Receiver<InferenceCommand>,
    event_tx: Sender<InferenceControlEvent>,
    result_ready_tx: Sender<()>,
    latest_result: Arc<Mutex<Option<InferenceFrameResult>>>,
    result_superseded: Arc<AtomicU64>,
    consumer: FrameConsumer,
    qpc: QpcClock,
    mut confidence_threshold: ConfidenceThreshold,
) {
    let mut pipeline: Option<crate::inference::pipeline::InferencePipeline> = None;
    let mut state = InferenceRunState::Idle;
    loop {
        if matches!(state, InferenceRunState::Idle) {
            match control_rx.recv() {
                Ok(command) => {
                    if !handle_command(
                        command,
                        &mut pipeline,
                        &mut state,
                        &consumer,
                        &mut confidence_threshold,
                    ) {
                        break;
                    }
                }
                Err(_) => break,
            }
            continue;
        }
        while let Ok(command) = control_rx.try_recv() {
            if !handle_command(
                command,
                &mut pipeline,
                &mut state,
                &consumer,
                &mut confidence_threshold,
            ) {
                return;
            }
        }
        let Some(pipeline_ref) = pipeline.as_mut() else {
            let generation = running_generation(&state);
            let _ = event_tx.send(InferenceControlEvent::Failed {
                generation,
                error: "inference pipeline is not prepared".to_owned(),
            });
            state = InferenceRunState::Idle;
            continue;
        };
        let Some(mut frame) = consumer.try_take_latest() else {
            select! {
                recv(control_rx) -> command => if let Ok(command) = command && !handle_command(command, &mut pipeline, &mut state, &consumer, &mut confidence_threshold) { return; },
                recv(consumer.ready_rx()) -> _ => {},
            }
            continue;
        };
        let generation = running_generation(&state);
        let metadata = *frame.metadata();
        if metadata.generation != generation {
            consumer.record_drop(GpuDropReason::StaleGeneration);
            continue;
        }
        let inference_start_qpc = qpc.now();
        let started = Instant::now();
        let result = pipeline_ref.infer(&mut frame);
        let report = if result.is_err() {
            pipeline_ref.take_fault_report()
        } else {
            None
        };
        let device_lost = pipeline_ref.device_lost();
        let inference_end_qpc = qpc.now();
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let poisoned = consumer.is_poisoned();
        drop(frame);
        match result {
            Ok(result) => {
                let completed = InferenceFrameResult {
                    metadata,
                    inference_start_qpc,
                    inference_end_qpc,
                    detection: result.detection,
                    ort_run_wall_ms: result.ort_run_wall_ms,
                    postprocess_ms: result.postprocess_ms,
                    gpu_preprocess_ms: result.gpu_preprocess_ms,
                };
                let mut latest = latest_result
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if latest.replace(completed).is_some() {
                    result_superseded.fetch_add(1, Ordering::Relaxed);
                }
                drop(latest);
                let _ = result_ready_tx.try_send(());
                tracing::trace!(elapsed_ms, "GPU frame inference finished");
            }
            Err(error) => {
                if poisoned {
                    let _ = event_tx.send(InferenceControlEvent::Poisoned {
                        generation,
                        error: format!("GPU pipeline failed: {error:#}"),
                        device_lost,
                        report,
                    });
                } else {
                    let _ = event_tx.send(InferenceControlEvent::Failed {
                        generation,
                        error: format!("inference failed: {error:#}"),
                    });
                }
                state = InferenceRunState::Idle;
            }
        }
    }
}

fn running_generation(state: &InferenceRunState) -> RunGeneration {
    match state {
        InferenceRunState::Running { generation } => *generation,
        InferenceRunState::Idle => unreachable!(),
    }
}

fn handle_command(
    command: InferenceCommand,
    pipeline: &mut Option<crate::inference::pipeline::InferencePipeline>,
    state: &mut InferenceRunState,
    consumer: &FrameConsumer,
    confidence_threshold: &mut ConfidenceThreshold,
) -> bool {
    match command {
        InferenceCommand::LoadModel {
            path,
            adapter_luid,
            capture_device,
            publisher,
            pipeline_id,
            reply,
        } => {
            let result = (|| {
                let mut prepared = prepare_directml_pipeline(
                    &path,
                    adapter_luid,
                    &capture_device,
                    publisher,
                    pipeline_id,
                )
                .map_err(|error| format!("{error:#}"))?;
                prepared
                    .pipeline
                    .set_confidence_threshold(*confidence_threshold);
                let input_size = (
                    prepared.pipeline.input_spec().width,
                    prepared.pipeline.input_spec().height,
                );
                let backend_info = prepared.pipeline.info().clone();
                let prepare_id = prepared.pipeline.prepare_id().to_owned();
                let info = PreparedModelInfo {
                    pipeline_id,
                    prepare_id,
                    input_size,
                    backend_info,
                    ep_assignment: prepared.ep_assignment,
                    capture_frames: prepared.capture_frames,
                };
                if let Some(old) = pipeline.replace(prepared.pipeline) {
                    drop(old);
                }
                Ok(info)
            })();
            let _ = reply.send(result);
        }
        InferenceCommand::SetConfidenceThreshold(value) => {
            *confidence_threshold = value;
            if let Some(pipeline) = pipeline.as_mut() {
                pipeline.set_confidence_threshold(value);
            }
        }
        InferenceCommand::Start { generation } => {
            *state = InferenceRunState::Running { generation }
        }
        InferenceCommand::Stop { generation, reply } => {
            if matches!(state, InferenceRunState::Running { generation: active } if *active == generation)
            {
                *state = InferenceRunState::Idle;
                consumer.clear();
            }
            let _ = reply.send(());
        }
        InferenceCommand::Shutdown => {
            *state = InferenceRunState::Idle;
            if let Some(mut active) = pipeline.take() {
                if let Err(error) = active.shutdown() {
                    tracing::warn!(error = %format!("{error:#}"), "inference pipeline shutdown failed");
                }
            }
            return false;
        }
    }
    true
}
