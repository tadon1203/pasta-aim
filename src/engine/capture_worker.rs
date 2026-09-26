use std::{
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use windows::Win32::Graphics::Direct3D11::ID3D11Device4;

use super::RunGeneration;
use crate::{
    graphics::{
        CaptureFramePool, CapturePoll, DxgiCapture, FrameConsumer, FramePublisher, GpuCopySample,
        GpuFaultReport, GpuFaultStage, GpuFrameMetadata, GraphicsThread, NoDesktopUpdateReason,
        frame_mailbox,
    },
    metrics::CaptureSourceMetrics,
    settings::FpsLimit,
    timing::QpcClock,
};

const ACQUIRE_TIMEOUT_MS: u32 = 2;
const CAPTURE_LOG_INTERVAL: Duration = Duration::from_secs(1);

pub(super) struct CaptureFault {
    pub generation: Option<RunGeneration>,
    pub error: String,
    pub report: Option<GpuFaultReport>,
    pub poisoned: bool,
    pub device_lost: bool,
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
        self.credit = if earned >= threshold {
            threshold
        } else {
            self.credit.saturating_add(earned)
        };
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
    ConfigureFramePool {
        pool: CaptureFramePool,
        reply: Sender<Result<(), String>>,
    },
    GetTransportDevice {
        reply: Sender<Result<ID3D11Device4, String>>,
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
        let thread_publisher = publisher.clone();
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
                        let _ = init_tx.send(Err(format!(
                            "capture COM MTA initialization failed: {error:#}"
                        )));
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
    pub(super) fn configure_frame_pool(&self, pool: CaptureFramePool) -> Result<()> {
        let (tx, rx) = bounded(1);
        self.control_tx
            .send(CaptureCommand::ConfigureFramePool { pool, reply: tx })?;
        rx.recv()?.map_err(anyhow::Error::msg)
    }
    pub(super) fn transport_device(&self) -> Result<ID3D11Device4> {
        let (tx, rx) = bounded(1);
        self.control_tx
            .send(CaptureCommand::GetTransportDevice { reply: tx })?;
        rx.recv()?.map_err(anyhow::Error::msg)
    }
    pub(super) fn publisher(&self) -> FramePublisher {
        self.publisher.clone()
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

    pub(super) fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = self.control_tx.send(CaptureCommand::Shutdown);
            let _ = join.join();
        }
    }
}

impl Drop for CaptureWorker {
    fn drop(&mut self) {
        self.shutdown();
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
    let mut transport: Option<CaptureFramePool> = None;
    let mut input_size: Option<(usize, usize)> = None;
    let mut state = CaptureThreadState::Idle;
    let mut stats = CaptureStats::default();
    let mut stats_started = Instant::now();
    let mut fps_gate = FrameRateGate::new(FpsLimit::default(), qpc);
    loop {
        if matches!(state, CaptureThreadState::Idle) {
            match control_rx.recv() {
                Ok(command) => {
                    if !handle_capture_command(
                        command,
                        &graphics,
                        &publisher,
                        &mut capture,
                        &mut transport,
                        &mut input_size,
                        &mut state,
                        &mut fps_gate,
                        &mut stats,
                        &mut stats_started,
                    ) {
                        break;
                    }
                }
                Err(_) => break,
            }
            continue;
        }
        while let Ok(command) = control_rx.try_recv() {
            if !handle_capture_command(
                command,
                &graphics,
                &publisher,
                &mut capture,
                &mut transport,
                &mut input_size,
                &mut state,
                &mut fps_gate,
                &mut stats,
                &mut stats_started,
            ) {
                return;
            }
        }
        // Commands drained above (e.g. Stop) may have returned the thread to Idle.
        let Some(generation) = capture_generation(&state) else {
            continue;
        };
        let Some((width, height)) = input_size else {
            state = CaptureThreadState::Idle;
            continue;
        };
        let Some(capture_ref) = capture.as_mut() else {
            send_fault(
                &event_tx,
                Some(generation),
                "capture is not configured".to_owned(),
            );
            state = CaptureThreadState::Idle;
            continue;
        };
        let Some(transport_ref) = transport.as_ref() else {
            send_fault(
                &event_tx,
                Some(generation),
                "GPU frame transport is not configured".to_owned(),
            );
            state = CaptureThreadState::Idle;
            continue;
        };
        if transport_ref.mailbox.is_poisoned() {
            state = CaptureThreadState::Idle;
            continue;
        }
        if let Err(error) = transport_ref.reclaim_completed() {
            let report = capture_gpu_fault(
                &transport_ref.mailbox,
                capture_ref,
                generation,
                GpuFaultStage::DeviceRemoved,
                None,
                None,
                None,
                None,
                None,
                None,
                &error,
            );

            transport_ref.mailbox.poison();
            send_poisoned_fault(
                &event_tx,
                Some(generation),
                format!("GPU frame transport failed: {error:#}"),
                true,
                Some(report),
            );
            state = CaptureThreadState::Idle;
            continue;
        }
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
                let completed = match transport_ref.completed_reuse_value() {
                    Ok(value) => value,
                    Err(error) => {
                        let report = capture_gpu_fault(
                            &transport_ref.mailbox,
                            capture_ref,
                            generation,
                            GpuFaultStage::DeviceRemoved,
                            None,
                            None,
                            None,
                            None,
                            None,
                            Some(u64::MAX),
                            &error,
                        );

                        transport_ref.mailbox.poison();
                        send_poisoned_fault(
                            &event_tx,
                            Some(generation),
                            format!("GPU frame transport failed: {error:#}"),
                            capture_ref.device_lost(),
                            Some(report),
                        );
                        state = CaptureThreadState::Idle;
                        continue;
                    }
                };
                let Some((slot_id, ready_value, frame_seq, overwrites)) =
                    transport_ref.mailbox.reserve(completed)
                else {
                    stats.gpu_slot_busy_drops += 1;
                    continue;
                };
                let submitted = capture_ref.copy_center_to(
                    &frame,
                    slot_id,
                    &transport_ref.textures[slot_id],
                    &transport_ref.capture_ready_fence,
                    ready_value,
                    width,
                    height,
                );
                let submitted = match submitted {
                    Ok(Some(submitted)) => submitted,
                    Ok(None) => {
                        transport_ref.mailbox.cancel_reservation(slot_id);
                        stats.gpu_slot_busy_drops += 1;
                        continue;
                    }
                    Err(error) => {
                        let report = capture_gpu_fault(
                            &transport_ref.mailbox,
                            capture_ref,
                            generation,
                            capture_ref.last_fault_stage(),
                            Some(slot_id),
                            Some(frame_seq),
                            Some(ready_value),
                            Some(0),
                            capture_ref.last_ready_completed(),
                            Some(completed),
                            &error,
                        );

                        transport_ref.mailbox.poison();
                        send_poisoned_fault(
                            &event_tx,
                            Some(generation),
                            format!("DXGI GPU copy failed: {error:#}"),
                            capture_ref.device_lost(),
                            Some(report),
                        );
                        state = CaptureThreadState::Idle;
                        continue;
                    }
                };
                let ready_qpc = qpc.now();
                let gpu_frame = GpuFrameMetadata {
                    pipeline_id: 0,
                    generation,
                    frame_seq,
                    slot_id,
                    present_qpc: submitted.present_qpc,
                    ready_qpc,
                    capture_ready_value: ready_value,

                    accumulated_frames: submitted.accumulated_frames,
                    desktop_size: submitted.desktop_size,
                    acquire_wait_ms: submitted.acquire_wait_ms,
                    capture_ms: submitted.capture_ms,
                    copy_submit_ms: submitted.copy_submit_ms,
                };
                if transport_ref.mailbox.publish(slot_id, gpu_frame) || overwrites {
                    stats.mailbox_overwrites += 1;
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
            let _ = event_tx.try_send(CaptureEvent::Stats(CaptureSourceMetrics {
                fresh_fps: fresh / elapsed,
                admitted_fps: admitted / elapsed,
                admission_drop_ratio: if stats.fresh_frames == 0 {
                    0.0
                } else {
                    stats.admission_drops as f64 / fresh
                },
                mean_accumulated_frames: if stats.fresh_frames == 0 {
                    0.0
                } else {
                    stats.accumulated_frames_sum as f64 / fresh
                },
                gpu_slot_busy_drops: stats.gpu_slot_busy_drops,
                mailbox_overwrites: stats.mailbox_overwrites,
                slots_in_flight: transport_ref.mailbox.slots_in_flight() as u64,
                gpu_drop_counts: transport_ref.mailbox.take_drop_counts(),
            }));
            stats = CaptureStats::default();
            stats_started = Instant::now();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_capture_command(
    command: CaptureCommand,
    graphics: &GraphicsThread,
    publisher: &FramePublisher,
    capture: &mut Option<DxgiCapture>,
    transport: &mut Option<CaptureFramePool>,
    input_size: &mut Option<(usize, usize)>,
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
        CaptureCommand::ConfigureFramePool {
            pool: new_transport,
            reply,
        } => {
            let result = (|| {
                if capture.is_none() {
                    return Err(anyhow!(
                        "capture output must be configured before its GPU transport"
                    ));
                }
                let mut desc = Default::default();
                unsafe { new_transport.textures[0].GetDesc(&mut desc) };
                let size = (desc.Width as usize, desc.Height as usize);
                *transport = Some(new_transport);
                *input_size = Some(size);
                publisher.clear();
                Ok(())
            })()
            .map_err(|error: anyhow::Error| format!("{error:#}"));
            let _ = reply.send(result);
        }
        CaptureCommand::GetTransportDevice { reply } => {
            let result = capture
                .as_ref()
                .map(DxgiCapture::transport_device)
                .ok_or_else(|| {
                    "capture output must be configured before its GPU transport".to_owned()
                });
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
    let _ = event_tx.send(CaptureEvent::Fault(CaptureFault {
        generation,
        error,
        report: None,
        poisoned: false,
        device_lost: false,
    }));
}

fn send_poisoned_fault(
    event_tx: &Sender<CaptureEvent>,
    generation: Option<RunGeneration>,
    error: String,
    device_lost: bool,
    report: Option<GpuFaultReport>,
) {
    let _ = event_tx.send(CaptureEvent::Fault(CaptureFault {
        generation,
        error,
        report,
        poisoned: true,
        device_lost,
    }));
}

#[allow(clippy::too_many_arguments)]
fn capture_gpu_fault(
    publisher: &FramePublisher,
    capture: &DxgiCapture,
    generation: RunGeneration,
    stage: GpuFaultStage,
    slot_id: Option<usize>,
    frame_seq: Option<u64>,
    ready_value: Option<u64>,
    reuse_value: Option<u64>,
    ready_completed: Option<u64>,
    reuse_completed: Option<u64>,
    error: &anyhow::Error,
) -> GpuFaultReport {
    let snapshot = publisher.diagnostic_snapshot();
    GpuFaultReport {
        pipeline_id: snapshot.pipeline_id,
        prepare_id: snapshot.prepare_id.clone(),
        generation: Some(generation),
        stage,
        slot_id,
        frame_seq,
        slot_state: slot_id.and_then(|id| snapshot.slots.get(id).map(|slot| slot.state)),
        ready_value,
        ready_completed,
        reuse_value,
        reuse_completed,
        hr: error
            .downcast_ref::<windows::core::Error>()
            .map(|error| error.code().0),
        d3d11_removed_reason: capture.device_removed_reason(),
        d3d12_removed_reason: None,
        error: format!("{error:#}"),
        transport: Some(snapshot),
        dred: None,
    }
}

#[derive(Default)]
struct CaptureStats {
    fresh_frames: u64,
    admitted_frames: u64,
    admission_drops: u64,
    timeouts: u64,
    pointer_only: u64,
    gpu_slot_busy_drops: u64,
    mailbox_overwrites: u64,
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
}
