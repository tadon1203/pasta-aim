use std::{
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};

use super::RunGeneration;
use crate::{
    detector::preprocess_bgra_into,
    graphics::{CapturePoll, DxgiCapture, GpuCopySample, GraphicsThread, NoDesktopUpdateReason},
    metrics::CaptureSourceMetrics,
    settings::FpsLimit,
    timing::QpcClock,
};

const ACQUIRE_TIMEOUT_MS: u32 = 2;
const INPUT_BUFFER_COUNT: usize = 3;
const CAPTURE_LOG_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy)]
pub(super) struct FrameMetadata {
    pub generation: RunGeneration,
    pub present_qpc: i64,
    pub ready_qpc: i64,
    pub accumulated_frames: u32,
    pub desktop_size: (usize, usize),
    pub acquire_wait_ms: f64,
    pub capture_ms: f64,
    pub copy_submit_ms: f64,
    pub map_wait_ms: f64,
    pub preprocess_ms: f64,
}

pub(super) struct PreparedFrame {
    metadata: FrameMetadata,
    input: Option<Box<[f32]>>,
    recycle_tx: Sender<Box<[f32]>>,
}

impl PreparedFrame {
    pub(super) fn metadata(&self) -> FrameMetadata {
        self.metadata
    }

    pub(super) fn input(&self) -> &[f32] {
        self.input.as_deref().expect("prepared frame input missing")
    }
}

impl Drop for PreparedFrame {
    fn drop(&mut self) {
        if let Some(buffer) = self.input.take() {
            let _ = self.recycle_tx.try_send(buffer);
        }
    }
}

struct FrameSlot {
    latest: Mutex<Option<PreparedFrame>>,
}

pub(super) struct FramePublisher {
    slot: Arc<FrameSlot>,
    ready_tx: Sender<()>,
}

pub(super) struct FrameConsumer {
    slot: Arc<FrameSlot>,
    ready_rx: Receiver<()>,
}

pub(super) fn frame_mailbox() -> (FramePublisher, FrameConsumer) {
    let slot = Arc::new(FrameSlot {
        latest: Mutex::new(None),
    });
    let (ready_tx, ready_rx) = bounded(1);
    (
        FramePublisher {
            slot: Arc::clone(&slot),
            ready_tx,
        },
        FrameConsumer { slot, ready_rx },
    )
}

impl FramePublisher {
    fn publish(&self, frame: PreparedFrame) -> bool {
        let old = self
            .slot
            .latest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(frame);
        let overwritten = old.is_some();
        drop(old);
        let _ = self.ready_tx.try_send(());
        overwritten
    }

    fn clear(&self) {
        self.slot
            .latest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }
}

impl FrameConsumer {
    pub(super) fn try_take_latest(&self) -> Option<PreparedFrame> {
        let _ = self.ready_rx.try_recv();
        let frame = self
            .slot
            .latest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        frame
    }

    pub(super) fn ready_rx(&self) -> &Receiver<()> {
        &self.ready_rx
    }

    pub(super) fn clear(&self) {
        self.slot
            .latest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        while self.ready_rx.try_recv().is_ok() {}
    }
}

pub(super) struct CaptureFault {
    pub generation: Option<RunGeneration>,
    pub error: String,
}

pub(super) enum CaptureEvent {
    Fault(CaptureFault),
    Stats(CaptureSourceMetrics),
    GpuCopy(GpuCopySample),
}

struct FrameRateGate {
    fps: FpsLimit,
    qpc_frequency: i64,
    last_present_qpc: Option<i64>,
    credit: i128,
}

impl FrameRateGate {
    fn new(fps: FpsLimit, qpc: QpcClock) -> Self {
        Self {
            fps,
            qpc_frequency: qpc.frequency_hz(),
            last_present_qpc: None,
            credit: 0,
        }
    }

    fn set_limit(&mut self, fps: FpsLimit) {
        self.fps = fps;
        self.reset();
    }

    fn reset(&mut self) {
        self.last_present_qpc = None;
        self.credit = 0;
    }

    fn admit(&mut self, present_qpc: i64) -> bool {
        let Some(last) = self.last_present_qpc.replace(present_qpc) else {
            return true;
        };
        let delta = present_qpc.saturating_sub(last).max(0) as i128;
        let threshold = self.qpc_frequency as i128;
        let earned = delta * i128::from(self.fps.value());
        if earned >= threshold {
            self.credit = threshold;
        } else {
            self.credit = self.credit.saturating_add(earned);
        }
        if self.credit >= threshold {
            self.credit -= threshold;
            true
        } else {
            false
        }
    }
}

enum CaptureCommand {
    ConfigureOutput {
        adapter: u32,
        output: u32,
        reply: Sender<Result<crate::graphics::DisplayInfo, String>>,
    },
    ConfigureInput {
        width: usize,
        height: usize,
        reply: Sender<Result<(), String>>,
    },
    StartContinuous {
        generation: RunGeneration,
        reply: Sender<Result<(), String>>,
    },
    CaptureOnce {
        generation: RunGeneration,
        reply: Sender<Result<(), String>>,
    },
    SetFpsLimit {
        value: FpsLimit,
        reply: Sender<()>,
    },
    Stop {
        reply: Sender<()>,
    },
    Shutdown,
}

enum CaptureThreadState {
    Idle,
    Continuous { generation: RunGeneration },
    OneShot { generation: RunGeneration },
}

pub(super) struct CaptureWorker {
    control_tx: Sender<CaptureCommand>,
    event_rx: Receiver<CaptureEvent>,
    join: Option<JoinHandle<()>>,
    publisher: FramePublisher,
}

impl CaptureWorker {
    pub(super) fn spawn(qpc: QpcClock) -> Result<(Self, FrameConsumer)> {
        let (control_tx, control_rx) = unbounded();
        let (event_tx, event_rx) = unbounded();
        let (publisher, consumer) = frame_mailbox();
        let thread_publisher = FramePublisher {
            slot: Arc::clone(&publisher.slot),
            ready_tx: publisher.ready_tx.clone(),
        };
        let (init_tx, init_rx) = bounded(1);
        let join = thread::Builder::new()
            .name("nice-aimassist-capture".to_owned())
            .spawn(move || {
                let graphics = match GraphicsThread::initialize() {
                    Ok(graphics) => {
                        let _ = init_tx.send(Ok(()));
                        graphics
                    }
                    Err(error) => {
                        let message = format!("capture COM MTA initialization failed: {error:#}");
                        let _ = init_tx.send(Err(message));
                        return;
                    }
                };
                capture_loop(control_rx, event_tx, thread_publisher, graphics, qpc);
            })?;
        match init_rx.recv() {
            Ok(Ok(())) => Ok((
                Self {
                    control_tx,
                    event_rx,
                    join: Some(join),
                    publisher,
                },
                consumer,
            )),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(anyhow!(error))
            }
            Err(error) => {
                let _ = join.join();
                Err(anyhow!(
                    "capture worker initialization handshake failed: {error}"
                ))
            }
        }
    }

    pub(super) fn configure_output(
        &self,
        adapter: u32,
        output: u32,
    ) -> Result<crate::graphics::DisplayInfo> {
        let (tx, rx) = bounded(1);
        self.control_tx.send(CaptureCommand::ConfigureOutput {
            adapter,
            output,
            reply: tx,
        })?;
        rx.recv()?.map_err(anyhow::Error::msg)
    }

    pub(super) fn configure_input(&self, width: usize, height: usize) -> Result<()> {
        let (tx, rx) = bounded(1);
        self.control_tx.send(CaptureCommand::ConfigureInput {
            width,
            height,
            reply: tx,
        })?;
        rx.recv()?.map_err(anyhow::Error::msg)
    }

    pub(super) fn start_continuous(&self, generation: RunGeneration) -> Result<()> {
        let (tx, rx) = bounded(1);
        self.control_tx.send(CaptureCommand::StartContinuous {
            generation,
            reply: tx,
        })?;
        rx.recv()?.map_err(anyhow::Error::msg)
    }

    pub(super) fn capture_once(&self, generation: RunGeneration) -> Result<()> {
        let (tx, rx) = bounded(1);
        self.control_tx.send(CaptureCommand::CaptureOnce {
            generation,
            reply: tx,
        })?;
        rx.recv()?.map_err(anyhow::Error::msg)
    }

    pub(super) fn set_fps_limit(&self, value: FpsLimit) -> Result<()> {
        let (tx, rx) = bounded(1);
        self.control_tx
            .send(CaptureCommand::SetFpsLimit { value, reply: tx })?;
        rx.recv()?;
        Ok(())
    }

    pub(super) fn stop(&self) -> Result<()> {
        let (tx, rx) = bounded(1);
        self.control_tx.send(CaptureCommand::Stop { reply: tx })?;
        rx.recv()?;
        self.publisher.clear();
        Ok(())
    }

    pub(super) fn event_rx(&self) -> &Receiver<CaptureEvent> {
        &self.event_rx
    }
}

impl Drop for CaptureWorker {
    fn drop(&mut self) {
        let _ = self.control_tx.send(CaptureCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn capture_loop(
    control_rx: Receiver<CaptureCommand>,
    event_tx: Sender<CaptureEvent>,
    publisher: FramePublisher,
    graphics: GraphicsThread,
    qpc: QpcClock,
) {
    let mut capture: Option<DxgiCapture> = None;
    let mut input_size: Option<(usize, usize)> = None;
    let mut buffers: Option<Receiver<Box<[f32]>>> = None;
    let mut recycle_tx: Option<Sender<Box<[f32]>>> = None;
    let mut state = CaptureThreadState::Idle;
    let mut stats = CaptureStats::default();
    let mut stats_started = Instant::now();
    let mut fps_gate = FrameRateGate::new(FpsLimit::default(), qpc);
    loop {
        if matches!(state, CaptureThreadState::Idle) {
            match control_rx.recv() {
                Ok(command) => {
                    if handle_capture_command(
                        command,
                        &graphics,
                        &publisher,
                        &mut capture,
                        &mut input_size,
                        &mut buffers,
                        &mut recycle_tx,
                        &mut state,
                        &mut fps_gate,
                        &mut stats,
                        &mut stats_started,
                    ) {
                        continue;
                    }
                    break;
                }
                Err(_) => break,
            }
        }

        while let Ok(command) = control_rx.try_recv() {
            if !handle_capture_command(
                command,
                &graphics,
                &publisher,
                &mut capture,
                &mut input_size,
                &mut buffers,
                &mut recycle_tx,
                &mut state,
                &mut fps_gate,
                &mut stats,
                &mut stats_started,
            ) {
                return;
            }
        }
        if matches!(state, CaptureThreadState::Idle) {
            continue;
        }
        let Some((width, height)) = input_size else {
            state = CaptureThreadState::Idle;
            continue;
        };
        let Some(capture_ref) = capture.as_mut() else {
            let generation = capture_generation(&state);
            send_fault(
                &event_tx,
                generation,
                "capture is not configured".to_owned(),
            );
            state = CaptureThreadState::Idle;
            continue;
        };
        let Some(buffer_rx) = buffers.as_ref() else {
            let generation = capture_generation(&state);
            send_fault(
                &event_tx,
                generation,
                "capture input buffer pool is not configured".to_owned(),
            );
            state = CaptureThreadState::Idle;
            continue;
        };
        let generation = capture_generation(&state).expect("active capture state has generation");
        match capture_ref.acquire(ACQUIRE_TIMEOUT_MS) {
            Ok(CapturePoll::NoDesktopUpdate(reason)) => match reason {
                NoDesktopUpdateReason::Timeout | NoDesktopUpdateReason::AccessLost => {
                    stats.timeouts += 1
                }
                NoDesktopUpdateReason::PointerOnly => stats.pointer_only += 1,
            },
            Ok(CapturePoll::Fresh(frame)) => {
                stats.fresh_frames += 1;
                stats.accumulated_frames_sum += u64::from(frame.accumulated_frames());
                if !matches!(state, CaptureThreadState::OneShot { .. })
                    && !fps_gate.admit(frame.present_qpc())
                {
                    stats.admission_drops += 1;
                    continue;
                }
                stats.admitted_frames += 1;
                let Some(mut input) = buffer_rx.try_recv().ok() else {
                    stats.buffer_starved_frames += 1;
                    continue;
                };
                let preprocess_started = Instant::now();
                let mapped = match capture_ref.copy_map_center(&frame, width, height) {
                    Ok(mapped) => mapped,
                    Err(error) => {
                        send_fault(
                            &event_tx,
                            Some(generation),
                            format!("capture copy/map failed: {error:#}"),
                        );
                        let _ = recycle_tx
                            .as_ref()
                            .expect("buffer pool sender")
                            .try_send(input);
                        state = CaptureThreadState::Idle;
                        continue;
                    }
                };
                if let Err(error) = preprocess_bgra_into(
                    mapped.bytes(),
                    mapped.row_pitch(),
                    width,
                    height,
                    &mut input,
                ) {
                    send_fault(
                        &event_tx,
                        Some(generation),
                        format!("preprocess failed: {error:#}"),
                    );
                    let _ = recycle_tx
                        .as_ref()
                        .expect("buffer pool sender")
                        .try_send(input);
                    state = CaptureThreadState::Idle;
                    continue;
                }
                let preprocess_ms = preprocess_started.elapsed().as_secs_f64() * 1000.0;
                let ready_qpc = qpc.now();
                let metadata = FrameMetadata {
                    generation,
                    present_qpc: mapped.present_qpc(),
                    ready_qpc,
                    accumulated_frames: mapped.accumulated_frames(),
                    desktop_size: mapped.desktop_size(),
                    acquire_wait_ms: mapped.acquire_wait_ms(),
                    capture_ms: mapped.capture_ms(),
                    copy_submit_ms: mapped.copy_submit_ms(),
                    map_wait_ms: mapped.map_wait_ms(),
                    preprocess_ms,
                };
                drop(mapped);
                let prepared = PreparedFrame {
                    metadata,
                    input: Some(input),
                    recycle_tx: recycle_tx.as_ref().expect("buffer pool sender").clone(),
                };
                if publisher.publish(prepared) {
                    stats.overwritten_frames += 1;
                }
                if let Some(sample) = capture_ref.try_take_gpu_copy_sample() {
                    let _ = event_tx.try_send(CaptureEvent::GpuCopy(sample));
                }
                if matches!(state, CaptureThreadState::OneShot { .. }) {
                    state = CaptureThreadState::Idle;
                }
            }
            Err(error) => {
                send_fault(
                    &event_tx,
                    Some(generation),
                    format!("DXGI capture failed: {error:#}"),
                );
                state = CaptureThreadState::Idle;
            }
        }

        if stats_started.elapsed() >= CAPTURE_LOG_INTERVAL {
            let elapsed = stats_started.elapsed().as_secs_f64();
            let fresh = stats.fresh_frames as f64;
            let admitted = stats.admitted_frames as f64;
            let drop_ratio = if stats.fresh_frames == 0 {
                0.0
            } else {
                stats.admission_drops as f64 / fresh
            };
            let _ = event_tx.try_send(CaptureEvent::Stats(CaptureSourceMetrics {
                fresh_fps: fresh / elapsed,
                admitted_fps: admitted / elapsed,
                admission_drop_ratio: drop_ratio,
                mean_accumulated_frames: if stats.fresh_frames == 0 {
                    0.0
                } else {
                    stats.accumulated_frames_sum as f64 / fresh
                },
                overwritten_frames: stats.overwritten_frames,
                buffer_starved_frames: stats.buffer_starved_frames,
            }));
            stats = CaptureStats::default();
            stats_started = Instant::now();
        }
    }
}

fn handle_capture_command(
    command: CaptureCommand,
    graphics: &GraphicsThread,
    publisher: &FramePublisher,
    capture: &mut Option<DxgiCapture>,
    input_size: &mut Option<(usize, usize)>,
    buffers: &mut Option<Receiver<Box<[f32]>>>,
    recycle_tx: &mut Option<Sender<Box<[f32]>>>,
    state: &mut CaptureThreadState,
    fps_gate: &mut FrameRateGate,
    stats: &mut CaptureStats,
    stats_started: &mut Instant,
) -> bool {
    match command {
        CaptureCommand::ConfigureOutput {
            adapter,
            output,
            reply,
        } => {
            let result = graphics
                .create_capture(adapter, output)
                .map(|new_capture| {
                    let info = new_capture.info().clone();
                    *capture = Some(new_capture);
                    info
                })
                .map_err(|error| format!("{error:#}"));
            let _ = reply.send(result);
        }
        CaptureCommand::ConfigureInput {
            width,
            height,
            reply,
        } => {
            let result = (|| {
                if width == 0 || height == 0 {
                    return Err("capture input size must be non-zero".to_owned());
                }
                let len = width
                    .checked_mul(height)
                    .and_then(|value| value.checked_mul(3))
                    .ok_or_else(|| "capture input size overflowed".to_owned())?;
                let (tx, rx) = bounded(INPUT_BUFFER_COUNT);
                for _ in 0..INPUT_BUFFER_COUNT {
                    tx.send(vec![0.0_f32; len].into_boxed_slice())
                        .map_err(|error| error.to_string())?;
                }
                *input_size = Some((width, height));
                *buffers = Some(rx);
                *recycle_tx = Some(tx);
                publisher.clear();
                Ok(())
            })();
            let _ = reply.send(result);
        }
        CaptureCommand::StartContinuous { generation, reply } => {
            publisher.clear();
            fps_gate.reset();
            *stats = CaptureStats::default();
            *stats_started = Instant::now();
            if let Some(capture) = capture.as_mut() {
                capture.reset_gpu_profiler();
            }
            *state = CaptureThreadState::Continuous { generation };
            let _ = reply.send(Ok(()));
        }
        CaptureCommand::CaptureOnce { generation, reply } => {
            publisher.clear();
            *state = CaptureThreadState::OneShot { generation };
            let _ = reply.send(Ok(()));
        }
        CaptureCommand::SetFpsLimit { value, reply } => {
            fps_gate.set_limit(value);
            let _ = reply.send(());
        }
        CaptureCommand::Stop { reply } => {
            *state = CaptureThreadState::Idle;
            publisher.clear();
            let _ = reply.send(());
        }
        CaptureCommand::Shutdown => return false,
    }
    true
}

fn capture_generation(state: &CaptureThreadState) -> Option<RunGeneration> {
    match state {
        CaptureThreadState::Idle => None,
        CaptureThreadState::Continuous { generation }
        | CaptureThreadState::OneShot { generation } => Some(*generation),
    }
}

fn send_fault(event_tx: &Sender<CaptureEvent>, generation: Option<RunGeneration>, error: String) {
    let _ = event_tx.send(CaptureEvent::Fault(CaptureFault { generation, error }));
}

#[derive(Default)]
struct CaptureStats {
    fresh_frames: u64,
    admitted_frames: u64,
    admission_drops: u64,
    timeouts: u64,
    pointer_only: u64,
    overwritten_frames: u64,
    buffer_starved_frames: u64,
    accumulated_frames_sum: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admitted_frames(source_hz: i64, limit: u16, frames: usize) -> usize {
        let qpc = QpcClock::test(source_hz as f64);
        let mut gate = FrameRateGate::new(FpsLimit::new(limit), qpc);
        (0..frames)
            .filter(|frame| gate.admit(*frame as i64))
            .count()
    }

    #[test]
    fn frame_rate_gate_accepts_first_frame_and_matches_source_limit() {
        assert_eq!(admitted_frames(120, 120, 120), 120);
        assert!((59..=61).contains(&admitted_frames(120, 60, 120)));
        assert!((99..=101).contains(&admitted_frames(120, 100, 120)));
        assert_eq!(admitted_frames(60, 120, 60), 60);
    }

    #[test]
    fn frame_rate_gate_does_not_burst_after_idle() {
        let qpc = QpcClock::test(120.0);
        let mut gate = FrameRateGate::new(FpsLimit::new(60), qpc);
        assert!(gate.admit(0));
        assert!(!gate.admit(1));
        assert!(gate.admit(2));
        assert!(gate.admit(1000));
        assert!(!gate.admit(1001));
    }

    #[test]
    fn frame_rate_gate_limit_change_resets_phase() {
        let qpc = QpcClock::test(120.0);
        let mut gate = FrameRateGate::new(FpsLimit::new(60), qpc);
        assert!(gate.admit(0));
        assert!(!gate.admit(1));
        gate.set_limit(FpsLimit::new(120));
        assert!(gate.admit(2));
    }

    fn frame(value: f32, recycle_tx: &Sender<Box<[f32]>>) -> PreparedFrame {
        PreparedFrame {
            metadata: FrameMetadata {
                generation: 1,
                present_qpc: 1,
                ready_qpc: 2,
                accumulated_frames: 1,
                desktop_size: (1, 1),
                acquire_wait_ms: 0.0,
                capture_ms: 0.0,
                copy_submit_ms: 0.0,
                map_wait_ms: 0.0,
                preprocess_ms: 0.0,
            },
            input: Some(vec![value].into_boxed_slice()),
            recycle_tx: recycle_tx.clone(),
        }
    }

    #[test]
    fn mailbox_keeps_only_latest_frame() {
        let (publisher, consumer) = frame_mailbox();
        let (recycle_tx, recycle_rx) = bounded(4);
        assert!(!publisher.publish(frame(1.0, &recycle_tx)));
        assert!(publisher.publish(frame(2.0, &recycle_tx)));
        assert_eq!(consumer.try_take_latest().unwrap().input(), &[2.0]);
        assert!(recycle_rx.try_recv().is_ok());
    }
}
