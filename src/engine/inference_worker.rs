use std::{
    path::PathBuf,
    thread::{self, JoinHandle},
};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};

use super::{
    RunGeneration,
    capture_worker::{FrameConsumer, FrameMetadata},
};
use crate::{
    detector::{Detection, Detector},
    settings::DirectMlDevice,
    timing::QpcClock,
};

pub(super) struct LoadedModelInfo {
    pub input_size: (usize, usize),
    pub directml_device: DirectMlDevice,
    pub ep_assignment: crate::detector::EpAssignmentState,
}

pub(super) struct InferenceFrameResult {
    pub metadata: FrameMetadata,
    pub inference_start_qpc: i64,
    pub inference_end_qpc: i64,
    pub detection: Option<Detection>,
    pub inference_ms: f64,
    pub postprocess_ms: f64,
}

pub(super) enum InferenceEvent {
    Completed(InferenceFrameResult),
    Failed {
        generation: RunGeneration,
        error: String,
    },
}

enum InferenceCommand {
    LoadModel {
        path: PathBuf,
        device: DirectMlDevice,
        reply: Sender<Result<LoadedModelInfo, String>>,
    },
    Start {
        generation: RunGeneration,
    },
    Stop {
        generation: RunGeneration,
    },
    Shutdown,
}

enum InferenceRunState {
    Idle,
    Running { generation: RunGeneration },
}

pub(super) struct InferenceWorker {
    control_tx: Sender<InferenceCommand>,
    event_rx: Receiver<InferenceEvent>,
    join: Option<JoinHandle<()>>,
}

impl InferenceWorker {
    pub(super) fn spawn(qpc: QpcClock, consumer: FrameConsumer) -> Result<Self> {
        let (control_tx, control_rx) = unbounded();
        let (event_tx, event_rx) = unbounded();
        let join = thread::Builder::new()
            .name("nice-aimassist-inference".to_owned())
            .spawn(move || inference_loop(control_rx, event_tx, consumer, qpc))?;
        Ok(Self {
            control_tx,
            event_rx,
            join: Some(join),
        })
    }

    pub(super) fn load_model(
        &self,
        path: PathBuf,
        device: DirectMlDevice,
    ) -> Result<LoadedModelInfo> {
        let (tx, rx) = bounded(1);
        self.control_tx.send(InferenceCommand::LoadModel {
            path,
            device,
            reply: tx,
        })?;
        rx.recv()?.map_err(anyhow::Error::msg)
    }

    pub(super) fn start(&self, generation: RunGeneration) -> Result<()> {
        self.control_tx
            .send(InferenceCommand::Start { generation })?;
        Ok(())
    }

    pub(super) fn stop(&self, generation: RunGeneration) -> Result<()> {
        self.control_tx
            .send(InferenceCommand::Stop { generation })?;
        Ok(())
    }

    pub(super) fn event_rx(&self) -> &Receiver<InferenceEvent> {
        &self.event_rx
    }
}

impl Drop for InferenceWorker {
    fn drop(&mut self) {
        let _ = self.control_tx.send(InferenceCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn inference_loop(
    control_rx: Receiver<InferenceCommand>,
    event_tx: Sender<InferenceEvent>,
    consumer: FrameConsumer,
    qpc: QpcClock,
) {
    let mut detector: Option<Detector> = None;
    let mut state = InferenceRunState::Idle;
    loop {
        if matches!(state, InferenceRunState::Idle) {
            match control_rx.recv() {
                Ok(command) => {
                    if !handle_command(command, &mut detector, &mut state, &consumer) {
                        break;
                    }
                }
                Err(_) => break,
            }
            continue;
        }

        while let Ok(command) = control_rx.try_recv() {
            if !handle_command(command, &mut detector, &mut state, &consumer) {
                return;
            }
        }
        let Some(detector_ref) = detector.as_mut() else {
            let generation = match state {
                InferenceRunState::Running { generation, .. } => generation,
                InferenceRunState::Idle => unreachable!(),
            };
            let _ = event_tx.send(InferenceEvent::Failed {
                generation,
                error: "inference model is not loaded".to_owned(),
            });
            state = InferenceRunState::Idle;
            continue;
        };
        let Some(frame) = consumer.try_take_latest() else {
            select! {
                recv(control_rx) -> command => if let Ok(command) = command { if !handle_command(command, &mut detector, &mut state, &consumer) { return; } },
                recv(consumer.ready_rx()) -> _ => {},
            }
            continue;
        };
        let metadata = frame.metadata();
        let generation = match state {
            InferenceRunState::Running { generation, .. } => generation,
            InferenceRunState::Idle => continue,
        };
        if metadata.generation != generation {
            tracing::debug!(
                frame_generation = metadata.generation,
                active_generation = generation,
                "discarding stale capture frame"
            );
            continue;
        }
        let inference_start_qpc = qpc.now();
        let result = detector_ref.infer_preprocessed(frame.input());
        let inference_end_qpc = qpc.now();
        let _ = frame;
        match result {
            Ok(result) => {
                let _ = event_tx.send(InferenceEvent::Completed(InferenceFrameResult {
                    metadata,
                    inference_start_qpc,
                    inference_end_qpc,
                    detection: result.detection,
                    inference_ms: result.inference_ms,
                    postprocess_ms: result.postprocess_ms,
                }));
            }
            Err(error) => {
                let _ = event_tx.send(InferenceEvent::Failed {
                    generation,
                    error: format!("inference failed: {error:#}"),
                });
                state = InferenceRunState::Idle;
            }
        }
    }
}

fn handle_command(
    command: InferenceCommand,
    detector: &mut Option<Detector>,
    state: &mut InferenceRunState,
    consumer: &FrameConsumer,
) -> bool {
    match command {
        InferenceCommand::LoadModel {
            path,
            device,
            reply,
        } => {
            let result = Detector::load(&path, device)
                .map(|new_detector| {
                    let info = LoadedModelInfo {
                        input_size: new_detector.input_size(),
                        directml_device: new_detector.directml_device(),
                        ep_assignment: new_detector.ep_assignment().clone(),
                    };
                    *detector = Some(new_detector);
                    info
                })
                .map_err(|error| format!("{error:#}"));
            let _ = reply.send(result);
        }
        InferenceCommand::Start { generation } => {
            // CaptureWorker clears the mailbox before starting a new generation.
            // Do not clear here: a fresh frame for this generation may already
            // have been published while this command was waiting behind a Run.
            *state = InferenceRunState::Running { generation };
        }
        InferenceCommand::Stop { generation } => {
            if matches!(state, InferenceRunState::Running { generation: active, .. } if *active == generation)
            {
                *state = InferenceRunState::Idle;
                consumer.clear();
            }
        }
        InferenceCommand::Shutdown => return false,
    }
    true
}
