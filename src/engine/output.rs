//! Turns detections into MAK output on the inference thread.
//!
//! The engine thread only configures this stage (settings via the inference
//! command channel, MAK routes through the shared client); the per-frame path
//! never leaves the inference thread.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::Result;

use crate::{
    inference::Detection,
    mak::{MakClient, PhysicalControllerStatus},
    settings::{OutputMode, Sensitivity, TargetYOffset},
    sync::MutexExt,
};

/// How long a one-shot controller command is held before the stick is released.
const CONTROLLER_ONE_SHOT_PULSE: Duration = Duration::from_millis(16);

/// User-tunable output parameters, sent to the inference thread on change.
#[derive(Debug, Clone, Copy)]
pub(super) struct OutputSettings {
    pub sensitivity: Sensitivity,
    pub target_y_offset: TargetYOffset,
    pub mode: OutputMode,
}

/// Rejects frames that were presented before the last output finished: they
/// cannot reflect the motion that output caused. Shared between the capture
/// thread (drops before the GPU copy) and the inference thread (drops before
/// inference).
#[derive(Clone)]
pub(super) struct MotionGate(Arc<GateState>);

struct GateState {
    last_complete_qpc: AtomicI64,
    discards: AtomicU64,
}

const NO_MOTION: i64 = i64::MIN;

impl MotionGate {
    pub(super) fn new() -> Self {
        Self(Arc::new(GateState {
            last_complete_qpc: AtomicI64::new(NO_MOTION),
            discards: AtomicU64::new(0),
        }))
    }

    pub(super) fn reset(&self) {
        self.0.last_complete_qpc.store(NO_MOTION, Ordering::Release);
    }

    pub(super) fn mark_complete(&self, qpc: i64) {
        self.0.last_complete_qpc.store(qpc, Ordering::Release);
    }

    /// Counts a discard when the frame is stale.
    pub(super) fn reject(&self, present_qpc: i64) -> bool {
        let last = self.0.last_complete_qpc.load(Ordering::Acquire);
        let stale = last != NO_MOTION && present_qpc <= last;
        if stale {
            self.0.discards.fetch_add(1, Ordering::Relaxed);
        }
        stale
    }

    pub(super) fn take_discards(&self) -> u64 {
        self.0.discards.swap(0, Ordering::AcqRel)
    }
}

/// `(dx, dy, confidence, class_id)` relative to the model input center.
pub(super) type TargetInfo = (f32, f32, f32, i32);
/// `(x, y, mode)` actually submitted to MAK.
pub(super) type CommandInfo = (i32, i32, OutputMode);

pub(super) struct OutputOutcome {
    pub target: Option<TargetInfo>,
    pub command: Option<CommandInfo>,
    /// A one-shot controller command needs its stick released after the pulse.
    pub pulse: bool,
}

/// Movement to submit for a detection, before it is routed to a MAK device.
#[derive(Debug, PartialEq)]
struct Plan {
    target: TargetInfo,
    /// `None` when the movement rounds to zero.
    step: Option<(i32, i32)>,
}

fn quantize(value: f32) -> i32 {
    value.round().clamp(i16::MIN as f32, i16::MAX as f32) as i32
}

fn plan(detection: Detection, input_size: (usize, usize), settings: OutputSettings) -> Plan {
    let dx = detection.center_x - input_size.0 as f32 * 0.5;
    let dy = detection.center_y - input_size.1 as f32 * 0.5;
    let gain = settings.sensitivity.gain();
    let relative_y_offset = detection.box_height * settings.target_y_offset.value() as f32 / 100.0;
    let x = quantize(dx * gain);
    let y = quantize(dy * gain + relative_y_offset);
    Plan {
        target: (dx, dy, detection.confidence, detection.class_id),
        step: (x != 0 || y != 0).then_some((x, y)),
    }
}

pub(super) struct OutputStage {
    mak: Arc<Mutex<MakClient>>,
    settings: OutputSettings,
    input_size: (usize, usize),
}

impl OutputStage {
    pub(super) fn new(mak: Arc<Mutex<MakClient>>, settings: OutputSettings) -> Self {
        Self {
            mak,
            settings,
            input_size: (0, 0),
        }
    }

    pub(super) fn set_settings(&mut self, settings: OutputSettings) {
        self.settings = settings;
    }

    pub(super) fn set_input_size(&mut self, input_size: (usize, usize)) {
        self.input_size = input_size;
    }

    /// Submits output for one frame. Without a usable movement the controller
    /// is released instead.
    pub(super) fn emit(
        &mut self,
        detection: Option<Detection>,
        one_shot: bool,
    ) -> Result<OutputOutcome> {
        let mut mak = self.mak.lock_recover();
        let Some(detection) = detection else {
            mak.release_controller()?;
            return Ok(OutputOutcome {
                target: None,
                command: None,
                pulse: false,
            });
        };
        let Plan { target, step } = plan(detection, self.input_size, self.settings);
        let Some((x, y)) = step else {
            mak.release_controller()?;
            return Ok(OutputOutcome {
                target: Some(target),
                command: None,
                pulse: false,
            });
        };
        let mode = self.settings.mode;
        match mode {
            OutputMode::Mouse => mak.move_mouse(x, y)?,
            OutputMode::Controller => mak.set_right_stick(x, y)?,
        }
        Ok(OutputOutcome {
            target: Some(target),
            command: Some((x, y, mode)),
            pulse: one_shot && mode == OutputMode::Controller,
        })
    }

    /// Holds a one-shot controller command for a moment, then releases it.
    pub(super) fn pulse_release(&mut self) -> Result<()> {
        thread::sleep(CONTROLLER_ONE_SHOT_PULSE);
        self.mak.lock_recover().release_controller()
    }

    pub(super) fn mak_snapshot(&self) -> MakSnapshot {
        let mak = self.mak.lock_recover();
        MakSnapshot {
            physical_controller: mak.physical_controller_status(),
            controller_release_pending: mak.controller_release_pending(),
        }
    }
}

/// MAK state that changes as a side effect of output.
#[derive(Debug, Clone, Copy)]
pub(super) struct MakSnapshot {
    pub physical_controller: PhysicalControllerStatus,
    pub controller_release_pending: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(center_y: f32, box_height: f32) -> Detection {
        Detection {
            center_x: 60.0,
            center_y,
            box_height,
            confidence: 0.9,
            class_id: 0,
        }
    }

    fn settings(sensitivity: u16, target_y_offset: i32) -> OutputSettings {
        OutputSettings {
            sensitivity: Sensitivity::new(sensitivity),
            target_y_offset: TargetYOffset::new(target_y_offset),
            mode: OutputMode::Mouse,
        }
    }

    #[test]
    fn quantize_uses_i16_bounds() {
        assert_eq!(quantize(1.4), 1);
        assert_eq!(quantize(-1.6), -2);
        assert_eq!(quantize(1.0e9), i16::MAX as i32);
        assert_eq!(quantize(-1.0e9), i16::MIN as i32);
    }

    #[test]
    fn target_y_offset_is_relative_to_target_height_and_added_after_sensitivity() {
        // Center is (50, 50); the detection is 10 px right and 20 px down.
        let plan = plan(detection(70.0, 40.0), (100, 100), settings(50, 25));
        assert_eq!(plan.step, Some((5, 20)));
    }

    #[test]
    fn zero_movement_produces_no_step() {
        let plan = plan(detection(50.0, 40.0), (100, 100), settings(100, 0));
        assert_eq!(plan.step, Some((10, 0)));
        let centered = Detection {
            center_x: 50.0,
            ..detection(50.0, 40.0)
        };
        assert_eq!(
            super::plan(centered, (100, 100), settings(100, 0)).step,
            None
        );
    }

    #[test]
    fn causal_gate_rejects_equal_and_older_present_times() {
        let gate = MotionGate::new();
        assert!(!gate.reject(0));
        gate.mark_complete(100);
        assert!(gate.reject(99));
        assert!(gate.reject(100));
        assert!(!gate.reject(101));
        assert_eq!(gate.take_discards(), 2);
        gate.reset();
        assert!(!gate.reject(0));
    }
}
