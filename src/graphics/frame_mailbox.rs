use crate::sync::MutexExt;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use crossbeam_channel::{Receiver, Sender, bounded};

use super::gpu_diagnostics::{GpuDropCounts, GpuDropReason};

pub const GPU_FRAME_SLOTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    Free,
    CaptureWriting,
    Published,
    Claimed,
    WaitingReuse { value: u64 },
}

#[derive(Debug, Clone, Copy)]
pub struct GpuFrameMetadata {
    pub pipeline_id: u64,
    pub generation: u64,
    pub frame_seq: u64,
    pub slot_id: usize,
    pub present_qpc: i64,
    pub ready_qpc: i64,
    pub capture_ready_value: u64,
    pub accumulated_frames: u32,
    pub desktop_size: (usize, usize),
    pub acquire_wait_ms: f64,
    pub capture_ms: f64,
    pub copy_submit_ms: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct SlotDiagnostic {
    pub slot_id: usize,
    pub state: SlotState,
    pub frame_seq: Option<u64>,
    pub ready_value: Option<u64>,
    pub reuse_value: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct TransportSnapshot {
    pub pipeline_id: u64,
    pub prepare_id: String,
    pub poisoned: bool,
    pub slots: [SlotDiagnostic; GPU_FRAME_SLOTS],
    pub drop_counts: GpuDropCounts,
}

struct Slot {
    state: SlotState,
    metadata: Option<GpuFrameMetadata>,
    frame_seq: Option<u64>,
    ready_value: Option<u64>,
    last_reuse_value: u64,
}

struct State {
    slots: [Slot; GPU_FRAME_SLOTS],
    latest_published: Option<usize>,
    next_frame_seq: u64,
    next_capture_ready_value: u64,
    next_reuse_value: u64,
    poisoned: bool,
    pipeline_id: u64,
    prepare_id: String,
    drop_counts: GpuDropCounts,
}

struct Shared {
    state: Mutex<State>,
    ready_tx: Sender<()>,
    ready_rx: Receiver<()>,
}

#[derive(Clone)]
pub struct FramePublisher(Arc<Shared>);
#[derive(Clone)]
pub struct FrameConsumer(Arc<Shared>);

pub fn frame_mailbox() -> (FramePublisher, FrameConsumer) {
    let (ready_tx, ready_rx) = bounded(1);
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            slots: std::array::from_fn(|_| Slot {
                state: SlotState::Free,
                metadata: None,
                frame_seq: None,
                ready_value: None,
                last_reuse_value: 0,
            }),
            latest_published: None,
            next_frame_seq: 1,
            next_capture_ready_value: 1,
            next_reuse_value: 1,
            poisoned: false,
            pipeline_id: 0,
            prepare_id: String::new(),
            drop_counts: GpuDropCounts::default(),
        }),
        ready_tx,
        ready_rx,
    });
    (FramePublisher(shared.clone()), FrameConsumer(shared))
}

impl FramePublisher {
    pub fn reserve(&self, reuse_completed_value: u64) -> Option<(usize, u64, u64, bool)> {
        let mut state = self.lock();
        if state.poisoned {
            state.drop_counts.increment(GpuDropReason::PipelinePoisoned);
            return None;
        }
        reclaim_locked(&mut state, reuse_completed_value);
        let free = state.slots.iter().position(|slot| {
            slot.state == SlotState::Free && slot.last_reuse_value <= reuse_completed_value
        });
        let (slot_id, overwritten) = match free {
            Some(id) => (id, false),
            None => match state.latest_published {
                Some(id) => (id, true),
                None => {
                    state.drop_counts.increment(GpuDropReason::NoFreeSlot);
                    return None;
                }
            },
        };
        if overwritten {
            state
                .drop_counts
                .increment(GpuDropReason::SupersededBeforeClaim);
            state.latest_published = None;
            transition_slot(&mut state, slot_id, SlotState::Free);
            state.slots[slot_id].metadata = None;
            state.slots[slot_id].frame_seq = None;
            state.slots[slot_id].ready_value = None;
        }
        let frame_seq = state.next_frame_seq;
        state.next_frame_seq = frame_seq.saturating_add(1);
        let ready_value = state.next_capture_ready_value;
        state.next_capture_ready_value = ready_value.saturating_add(1);
        state.slots[slot_id].metadata = None;
        state.slots[slot_id].frame_seq = Some(frame_seq);
        state.slots[slot_id].ready_value = Some(ready_value);
        transition_slot(&mut state, slot_id, SlotState::CaptureWriting);
        Some((slot_id, ready_value, frame_seq, overwritten))
    }

    pub fn publish(&self, slot_id: usize, mut metadata: GpuFrameMetadata) -> bool {
        let mut state = self.lock();
        if state.poisoned
            || slot_id >= GPU_FRAME_SLOTS
            || state.slots[slot_id].state != SlotState::CaptureWriting
        {
            return false;
        }
        let old = state.latest_published.take();
        if let Some(old_id) = old.filter(|old_id| *old_id != slot_id) {
            state
                .drop_counts
                .increment(GpuDropReason::SupersededBeforeClaim);
            transition_slot(&mut state, old_id, SlotState::Free);
            state.slots[old_id].metadata = None;
            state.slots[old_id].frame_seq = None;
            state.slots[old_id].ready_value = None;
        }
        metadata.pipeline_id = state.pipeline_id;
        state.slots[slot_id].frame_seq = Some(metadata.frame_seq);
        state.slots[slot_id].ready_value = Some(metadata.capture_ready_value);
        state.slots[slot_id].metadata = Some(metadata);
        transition_slot(&mut state, slot_id, SlotState::Published);
        state.latest_published = Some(slot_id);
        let _ = self.0.ready_tx.try_send(());
        old.is_some()
    }

    pub fn cancel_reservation(&self, slot_id: usize) {
        let mut state = self.lock();
        if !state.poisoned
            && slot_id < GPU_FRAME_SLOTS
            && state.slots[slot_id].state == SlotState::CaptureWriting
        {
            transition_slot(&mut state, slot_id, SlotState::Free);
            state.slots[slot_id].metadata = None;
            state.slots[slot_id].frame_seq = None;
            state.slots[slot_id].ready_value = None;
        }
    }

    pub fn reclaim(&self, completed_value: u64) -> Result<()> {
        let mut state = self.lock();
        if state.poisoned {
            bail!("GPU frame mailbox is poisoned");
        }
        reclaim_locked(&mut state, completed_value);
        Ok(())
    }

    pub fn poison(&self) {
        let mut state = self.lock();
        if !state.poisoned {
            state.drop_counts.increment(GpuDropReason::PipelinePoisoned);
            state.poisoned = true;
        }
    }

    pub fn set_pipeline_identity(&self, pipeline_id: u64, prepare_id: String) {
        let mut state = self.lock();
        state.pipeline_id = pipeline_id;
        state.prepare_id = prepare_id;
    }

    pub fn shared_fence_initial_values(&self) -> (u64, u64) {
        let state = self.lock();
        (
            state.next_capture_ready_value.saturating_sub(1),
            state.next_reuse_value.saturating_sub(1),
        )
    }

    pub fn diagnostic_snapshot(&self) -> TransportSnapshot {
        let state = self.lock();
        let slots = std::array::from_fn(|slot_id| {
            let slot = &state.slots[slot_id];
            SlotDiagnostic {
                slot_id,
                state: slot.state,
                frame_seq: slot.frame_seq,
                ready_value: slot.ready_value,
                reuse_value: match slot.state {
                    SlotState::WaitingReuse { value } => Some(value),
                    _ => None,
                },
            }
        });
        TransportSnapshot {
            pipeline_id: state.pipeline_id,
            prepare_id: state.prepare_id.clone(),
            poisoned: state.poisoned,
            slots,
            drop_counts: state.drop_counts,
        }
    }

    pub fn take_drop_counts(&self) -> GpuDropCounts {
        let mut state = self.lock();
        std::mem::take(&mut state.drop_counts)
    }

    pub fn is_poisoned(&self) -> bool {
        self.lock().poisoned
    }

    pub fn clear(&self) {
        let mut state = self.lock();
        if state.poisoned {
            return;
        }
        if let Some(id) = state.latest_published.take() {
            transition_slot(&mut state, id, SlotState::Free);
            state.slots[id].metadata = None;
            state.slots[id].frame_seq = None;
            state.slots[id].ready_value = None;
        }
        while self.0.ready_rx.try_recv().is_ok() {}
    }

    pub fn slots_in_flight(&self) -> usize {
        self.lock()
            .slots
            .iter()
            .filter(|slot| slot.state != SlotState::Free)
            .count()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock_recover()
    }
}

impl FrameConsumer {
    pub fn record_drop(&self, reason: GpuDropReason) {
        self.lock().drop_counts.increment(reason);
    }

    pub fn try_take_latest(&self) -> Option<GpuFrameLease> {
        let _ = self.0.ready_rx.try_recv();
        let mut state = self.lock();
        if state.poisoned {
            return None;
        }
        let slot_id = state.latest_published.take()?;
        if state.slots[slot_id].state != SlotState::Published {
            return None;
        }
        let metadata = state.slots[slot_id].metadata?;
        transition_slot(&mut state, slot_id, SlotState::Claimed);
        let reuse_value = state.next_reuse_value;
        state.next_reuse_value = reuse_value.saturating_add(1);
        Some(GpuFrameLease {
            shared: self.0.clone(),
            metadata,
            reuse_value,
            reuse_submitted: false,
        })
    }

    pub fn ready_rx(&self) -> &Receiver<()> {
        &self.0.ready_rx
    }

    pub fn is_poisoned(&self) -> bool {
        self.lock().poisoned
    }

    pub fn clear(&self) {
        let mut state = self.lock();
        if state.poisoned {
            return;
        }
        state.latest_published = None;
        for slot_id in 0..GPU_FRAME_SLOTS {
            if state.slots[slot_id].state == SlotState::Published {
                transition_slot(&mut state, slot_id, SlotState::Free);
                state.slots[slot_id].metadata = None;
                state.slots[slot_id].frame_seq = None;
                state.slots[slot_id].ready_value = None;
            }
        }
        while self.0.ready_rx.try_recv().is_ok() {}
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock_recover()
    }
}

pub struct GpuFrameLease {
    shared: Arc<Shared>,
    metadata: GpuFrameMetadata,
    reuse_value: u64,
    reuse_submitted: bool,
}

impl GpuFrameLease {
    pub fn metadata(&self) -> &GpuFrameMetadata {
        &self.metadata
    }
    pub fn slot_id(&self) -> usize {
        self.metadata.slot_id
    }
    pub fn capture_ready_value(&self) -> u64 {
        self.metadata.capture_ready_value
    }
    pub fn reuse_value(&self) -> u64 {
        self.reuse_value
    }
    pub fn record_drop(&self, reason: GpuDropReason) {
        self.shared
            .state
            .lock_recover()
            .drop_counts
            .increment(reason);
    }
    pub fn mark_reuse_submitted(&mut self) -> bool {
        let mut state = self.shared.state.lock_recover();
        if state.poisoned || self.metadata.slot_id >= GPU_FRAME_SLOTS {
            return false;
        }
        if state.slots[self.metadata.slot_id].state == SlotState::Claimed {
            transition_slot(
                &mut state,
                self.metadata.slot_id,
                SlotState::WaitingReuse {
                    value: self.reuse_value,
                },
            );
            state.slots[self.metadata.slot_id].last_reuse_value = self.reuse_value;
            self.reuse_submitted = true;
            true
        } else {
            false
        }
    }
}

impl Drop for GpuFrameLease {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock_recover();
        if state.poisoned || self.reuse_submitted || self.metadata.slot_id >= GPU_FRAME_SLOTS {
            return;
        }
        if state.slots[self.metadata.slot_id].state == SlotState::Claimed {
            transition_slot(&mut state, self.metadata.slot_id, SlotState::Free);
            state.slots[self.metadata.slot_id].metadata = None;
            state.slots[self.metadata.slot_id].frame_seq = None;
            state.slots[self.metadata.slot_id].ready_value = None;
        }
    }
}

fn reclaim_locked(state: &mut State, completed_value: u64) {
    if state.poisoned {
        return;
    }
    for slot_id in 0..GPU_FRAME_SLOTS {
        if matches!(state.slots[slot_id].state, SlotState::WaitingReuse { value } if value <= completed_value)
        {
            transition_slot(state, slot_id, SlotState::Free);
            state.slots[slot_id].metadata = None;
            state.slots[slot_id].frame_seq = None;
            state.slots[slot_id].ready_value = None;
        }
    }
}

fn transition_slot(state: &mut State, slot_id: usize, to: SlotState) {
    let slot = &state.slots[slot_id];
    let from = slot.state;
    if from == to {
        return;
    }
    tracing::trace!(
        slot_id,
        from = ?from,
        to = ?to,
        frame_seq = ?slot.frame_seq,
        ready_value = ?slot.ready_value,
        "GPU frame slot transition"
    );
    state.slots[slot_id].state = to;
}
