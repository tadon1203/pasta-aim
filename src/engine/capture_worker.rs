use std::{
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use windows::Win32::Graphics::Direct3D11::ID3D11Device4;

use super::{RunGeneration, RunMode, output::MotionGate};
use crate::{
    graphics::{
        AcquiredDesktopFrame, CaptureFramePool, CapturePoll, DisplayInfo, DxgiCapture,
        FrameConsumer, FramePublisher, GpuCopySample, GpuFaultReport, GpuFaultStage,
        GpuFrameMetadata, GraphicsThread, NoDesktopUpdateReason, frame_mailbox,
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
    Fault(Box<CaptureFault>),
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

type Reply<T> = Sender<Result<T, String>>;

enum CaptureCommand {
    ConfigureOutput {
        adapter: u32,
        output: u32,
        reply: Reply<DisplayInfo>,
    },
    ConfigureFramePool {
        pool: CaptureFramePool,
        reply: Reply<()>,
    },
    GetTransportDevice {
        reply: Reply<ID3D11Device4>,
    },
    Start {
        mode: RunMode,
        generation: RunGeneration,
        reply: Reply<()>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureThreadState {
    Idle,
    Running {
        mode: RunMode,
        generation: RunGeneration,
    },
}

pub(super) struct CaptureWorker {
    control_tx: Sender<CaptureCommand>,
    event_rx: Receiver<CaptureEvent>,
    join: Option<JoinHandle<()>>,
    publisher: FramePublisher,
}

impl CaptureWorker {
    pub(super) fn spawn(qpc: QpcClock, gate: MotionGate) -> Result<(Self, FrameConsumer)> {
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
                let _boost = crate::platform::boost_current_thread("capture");
                CaptureThread::new(control_rx, event_tx, thread_publisher, graphics, qpc, gate)
                    .run();
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

    /// Sends a command built around a reply channel and waits for the answer.
    fn request<T>(&self, command: impl FnOnce(Reply<T>) -> CaptureCommand) -> Result<T> {
        let (tx, rx) = bounded(1);
        self.control_tx.send(command(tx))?;
        rx.recv()?.map_err(anyhow::Error::msg)
    }

    pub(super) fn configure_output(&self, adapter: u32, output: u32) -> Result<DisplayInfo> {
        self.request(|reply| CaptureCommand::ConfigureOutput {
            adapter,
            output,
            reply,
        })
    }

    pub(super) fn configure_frame_pool(&self, pool: CaptureFramePool) -> Result<()> {
        self.request(|reply| CaptureCommand::ConfigureFramePool { pool, reply })
    }

    pub(super) fn transport_device(&self) -> Result<ID3D11Device4> {
        self.request(|reply| CaptureCommand::GetTransportDevice { reply })
    }

    pub(super) fn publisher(&self) -> FramePublisher {
        self.publisher.clone()
    }

    pub(super) fn start(&self, mode: RunMode, generation: RunGeneration) -> Result<()> {
        self.request(|reply| CaptureCommand::Start {
            mode,
            generation,
            reply,
        })
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

/// Where in the GPU transport a fault happened, for the fault report.
#[derive(Default)]
struct FaultDetail {
    slot_id: Option<usize>,
    frame_seq: Option<u64>,
    ready_value: Option<u64>,
    reuse_value: Option<u64>,
    ready_completed: Option<u64>,
    reuse_completed: Option<u64>,
}

struct CaptureThread {
    control_rx: Receiver<CaptureCommand>,
    event_tx: Sender<CaptureEvent>,
    publisher: FramePublisher,
    graphics: GraphicsThread,
    qpc: QpcClock,
    gate: MotionGate,
    capture: Option<DxgiCapture>,
    transport: Option<CaptureFramePool>,
    input_size: Option<(usize, usize)>,
    state: CaptureThreadState,
    stats: CaptureStats,
    stats_started: Instant,
    fps_gate: FrameRateGate,
}

impl CaptureThread {
    fn new(
        control_rx: Receiver<CaptureCommand>,
        event_tx: Sender<CaptureEvent>,
        publisher: FramePublisher,
        graphics: GraphicsThread,
        qpc: QpcClock,
        gate: MotionGate,
    ) -> Self {
        Self {
            control_rx,
            event_tx,
            publisher,
            graphics,
            qpc,
            gate,
            capture: None,
            transport: None,
            input_size: None,
            state: CaptureThreadState::Idle,
            stats: CaptureStats::default(),
            stats_started: Instant::now(),
            fps_gate: FrameRateGate::new(FpsLimit::default(), qpc),
        }
    }

    fn run(mut self) {
        loop {
            if self.state == CaptureThreadState::Idle {
                match self.control_rx.recv() {
                    Ok(command) => {
                        if !self.handle_command(command) {
                            return;
                        }
                    }
                    Err(_) => return,
                }
                continue;
            }
            while let Ok(command) = self.control_rx.try_recv() {
                if !self.handle_command(command) {
                    return;
                }
            }
            // Commands drained above (e.g. Stop) may have returned the thread to Idle.
            if self.state != CaptureThreadState::Idle {
                self.step();
            }
        }
    }

    /// Returns false when the thread should exit.
    fn handle_command(&mut self, command: CaptureCommand) -> bool {
        match command {
            CaptureCommand::ConfigureOutput {
                adapter,
                output,
                reply,
            } => {
                let result = self
                    .graphics
                    .create_capture(adapter, output)
                    .map(|new_capture| {
                        let info = new_capture.info().clone();
                        self.capture = Some(new_capture);
                        info
                    })
                    .map_err(|error| format!("{error:#}"));
                let _ = reply.send(result);
            }
            CaptureCommand::ConfigureFramePool { pool, reply } => {
                let result = if self.capture.is_none() {
                    Err("capture output must be configured before its GPU transport".to_owned())
                } else {
                    let mut desc = Default::default();
                    // SAFETY: the pool's textures are live and `desc` is writable.
                    unsafe { pool.textures[0].GetDesc(&mut desc) };
                    self.input_size = Some((desc.Width as usize, desc.Height as usize));
                    self.transport = Some(pool);
                    self.publisher.clear();
                    Ok(())
                };
                let _ = reply.send(result);
            }
            CaptureCommand::GetTransportDevice { reply } => {
                let result = self
                    .capture
                    .as_ref()
                    .map(DxgiCapture::transport_device)
                    .ok_or_else(|| {
                        "capture output must be configured before its GPU transport".to_owned()
                    });
                let _ = reply.send(result);
            }
            CaptureCommand::Start {
                mode,
                generation,
                reply,
            } => {
                self.publisher.clear();
                if mode == RunMode::Continuous {
                    self.fps_gate.reset();
                    self.stats = CaptureStats::default();
                    self.stats_started = Instant::now();
                    if let Some(capture) = self.capture.as_mut() {
                        capture.reset_gpu_profiler();
                    }
                }
                self.state = CaptureThreadState::Running { mode, generation };
                let _ = reply.send(Ok(()));
            }
            CaptureCommand::SetFpsLimit { value, reply } => {
                self.fps_gate.set_limit(value);
                let _ = reply.send(());
            }
            CaptureCommand::Stop { reply } => {
                self.state = CaptureThreadState::Idle;
                self.publisher.clear();
                let _ = reply.send(());
            }
            CaptureCommand::Shutdown => return false,
        }
        true
    }

    /// Reports a fault that leaves the GPU transport unusable and stops capturing.
    fn poison(
        &mut self,
        generation: RunGeneration,
        stage: GpuFaultStage,
        detail: FaultDetail,
        error: &anyhow::Error,
        message: String,
        device_lost: bool,
    ) {
        if let (Some(transport), Some(capture)) = (&self.transport, &self.capture) {
            let report = capture_gpu_fault(
                &transport.mailbox,
                capture,
                generation,
                stage,
                detail,
                error,
            );
            transport.mailbox.poison();
            send_poisoned_fault(
                &self.event_tx,
                Some(generation),
                message,
                device_lost,
                Some(report),
            );
        }
        self.state = CaptureThreadState::Idle;
    }

    fn fail(&mut self, generation: RunGeneration, message: String) {
        send_fault(&self.event_tx, Some(generation), message);
        self.state = CaptureThreadState::Idle;
    }

    /// Acquires at most one desktop frame and forwards it to the GPU transport.
    fn step(&mut self) {
        let CaptureThreadState::Running { mode, generation } = self.state else {
            return;
        };
        let Some(size) = self.input_size else {
            self.state = CaptureThreadState::Idle;
            return;
        };
        let (Some(capture), Some(transport)) = (self.capture.as_mut(), self.transport.as_ref())
        else {
            let missing = if self.capture.is_none() {
                "capture is not configured"
            } else {
                "GPU frame transport is not configured"
            };
            return self.fail(generation, missing.to_owned());
        };
        if transport.mailbox.is_poisoned() {
            self.state = CaptureThreadState::Idle;
            return;
        }
        if let Err(error) = transport.reclaim_completed() {
            let message = format!("GPU frame transport failed: {error:#}");
            return self.poison(
                generation,
                GpuFaultStage::DeviceRemoved,
                FaultDetail::default(),
                &error,
                message,
                true,
            );
        }
        match capture.acquire(ACQUIRE_TIMEOUT_MS) {
            Ok(CapturePoll::NoDesktopUpdate(reason)) => match reason {
                NoDesktopUpdateReason::Timeout | NoDesktopUpdateReason::AccessLost => {
                    self.stats.timeouts += 1
                }
                NoDesktopUpdateReason::PointerOnly => self.stats.pointer_only += 1,
            },
            Ok(CapturePoll::Fresh(frame)) => self.forward_frame(mode, generation, size, &frame),
            Err(error) => self.fail(generation, format!("DXGI capture failed: {error:#}")),
        }
        self.report_stats_if_due();
    }

    fn forward_frame(
        &mut self,
        mode: RunMode,
        generation: RunGeneration,
        size: (usize, usize),
        frame: &AcquiredDesktopFrame,
    ) {
        self.stats.fresh_frames += 1;
        self.stats.accumulated_frames_sum += u64::from(frame.accumulated_frames());
        // Presented before the last output finished, so it cannot show that motion.
        if mode == RunMode::Continuous && self.gate.reject(frame.present_qpc()) {
            return;
        }
        if mode == RunMode::Continuous && !self.fps_gate.admit(frame.present_qpc()) {
            self.stats.admission_drops += 1;
            return;
        }
        self.stats.admitted_frames += 1;

        let (Some(capture), Some(transport)) = (self.capture.as_mut(), self.transport.as_ref())
        else {
            return;
        };
        let completed = match transport.completed_reuse_value() {
            Ok(value) => value,
            Err(error) => {
                let detail = FaultDetail {
                    reuse_completed: Some(u64::MAX),
                    ..FaultDetail::default()
                };
                let message = format!("GPU frame transport failed: {error:#}");
                let device_lost = capture.device_lost();
                return self.poison(
                    generation,
                    GpuFaultStage::DeviceRemoved,
                    detail,
                    &error,
                    message,
                    device_lost,
                );
            }
        };
        let Some((slot_id, ready_value, frame_seq, overwrites)) =
            transport.mailbox.reserve(completed)
        else {
            self.stats.gpu_slot_busy_drops += 1;
            return;
        };
        let submitted = capture.copy_center_to(
            frame,
            slot_id,
            &transport.textures[slot_id],
            &transport.capture_ready_fence,
            ready_value,
            size,
        );
        let submitted = match submitted {
            Ok(Some(submitted)) => submitted,
            Ok(None) => {
                transport.mailbox.cancel_reservation(slot_id);
                self.stats.gpu_slot_busy_drops += 1;
                return;
            }
            Err(error) => {
                let stage = capture.last_fault_stage();
                let detail = FaultDetail {
                    slot_id: Some(slot_id),
                    frame_seq: Some(frame_seq),
                    ready_value: Some(ready_value),
                    reuse_value: Some(0),
                    ready_completed: capture.last_ready_completed(),
                    reuse_completed: Some(completed),
                };
                let message = format!("DXGI GPU copy failed: {error:#}");
                let device_lost = capture.device_lost();
                return self.poison(generation, stage, detail, &error, message, device_lost);
            }
        };
        let gpu_frame = GpuFrameMetadata {
            pipeline_id: 0,
            generation,
            frame_seq,
            slot_id,
            present_qpc: submitted.present_qpc,
            ready_qpc: self.qpc.now(),
            capture_ready_value: ready_value,
            accumulated_frames: submitted.accumulated_frames,
            desktop_size: submitted.desktop_size,
            acquire_wait_ms: submitted.acquire_wait_ms,
            capture_ms: submitted.capture_ms,
            copy_submit_ms: submitted.copy_submit_ms,
        };
        if transport.mailbox.publish(slot_id, gpu_frame) || overwrites {
            self.stats.mailbox_overwrites += 1;
        }
        if let Some(sample) = capture.try_take_gpu_copy_sample() {
            let _ = self.event_tx.try_send(CaptureEvent::GpuCopy(sample));
        }
        if mode == RunMode::OneShot {
            self.state = CaptureThreadState::Idle;
        }
    }

    fn report_stats_if_due(&mut self) {
        let elapsed = self.stats_started.elapsed();
        if elapsed < CAPTURE_LOG_INTERVAL {
            return;
        }
        let Some(transport) = self.transport.as_ref() else {
            return;
        };
        let stats = std::mem::take(&mut self.stats);
        let seconds = elapsed.as_secs_f64();
        let fresh = stats.fresh_frames as f64;
        let per_fresh = |value: f64| {
            if stats.fresh_frames == 0 {
                0.0
            } else {
                value / fresh
            }
        };
        let _ = self
            .event_tx
            .try_send(CaptureEvent::Stats(CaptureSourceMetrics {
                fresh_fps: fresh / seconds,
                admitted_fps: stats.admitted_frames as f64 / seconds,
                admission_drop_ratio: per_fresh(stats.admission_drops as f64),
                mean_accumulated_frames: per_fresh(stats.accumulated_frames_sum as f64),
                gpu_slot_busy_drops: stats.gpu_slot_busy_drops,
                mailbox_overwrites: stats.mailbox_overwrites,
                slots_in_flight: transport.mailbox.slots_in_flight() as u64,
                gpu_drop_counts: transport.mailbox.take_drop_counts(),
            }));
        self.stats_started = Instant::now();
    }
}

fn send_fault(event_tx: &Sender<CaptureEvent>, generation: Option<RunGeneration>, error: String) {
    let _ = event_tx.send(CaptureEvent::Fault(Box::new(CaptureFault {
        generation,
        error,
        report: None,
        poisoned: false,
        device_lost: false,
    })));
}

fn send_poisoned_fault(
    event_tx: &Sender<CaptureEvent>,
    generation: Option<RunGeneration>,
    error: String,
    device_lost: bool,
    report: Option<GpuFaultReport>,
) {
    let _ = event_tx.send(CaptureEvent::Fault(Box::new(CaptureFault {
        generation,
        error,
        report,
        poisoned: true,
        device_lost,
    })));
}

fn capture_gpu_fault(
    publisher: &FramePublisher,
    capture: &DxgiCapture,
    generation: RunGeneration,
    stage: GpuFaultStage,
    detail: FaultDetail,
    error: &anyhow::Error,
) -> GpuFaultReport {
    let snapshot = publisher.diagnostic_snapshot();
    GpuFaultReport {
        pipeline_id: snapshot.pipeline_id,
        prepare_id: snapshot.prepare_id.clone(),
        generation: Some(generation),
        stage,
        slot_id: detail.slot_id,
        frame_seq: detail.frame_seq,
        slot_state: detail
            .slot_id
            .and_then(|id| snapshot.slots.get(id).map(|slot| slot.state)),
        ready_value: detail.ready_value,
        ready_completed: detail.ready_completed,
        reuse_value: detail.reuse_value,
        reuse_completed: detail.reuse_completed,
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
