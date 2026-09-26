mod binding;
mod capture;
mod xinput;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use eframe::egui;
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey},
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

use crate::{
    engine::EngineControl,
    settings::{HotkeyAction, HotkeyBinding},
};

pub use binding::{canonical_shortcut, display_shortcut};
use binding::{display_hotkey, parse_bindings};
pub use capture::{CaptureResult, capture_from_events};
use xinput::{EdgeState, Poller as XInputPoller, SLOT_NONE, XInputControl};

const LISTENER_MAX_SLEEP: Duration = Duration::from_millis(50);
const KEYBOARD_RELEASE_PROBE_INTERVAL: Duration = Duration::from_millis(2);
const RELEASE_METRIC_SAMPLES: usize = 128;
const RELEASE_ASSIST_MIN_SAMPLES: usize = 8;
const RELEASE_ASSIST_P95_THRESHOLD_MS: f64 = 25.0;
const RELEASE_OBSERVATION_TTL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TriggerState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TriggerId {
    Keyboard(u32),
    XInput(XInputControl),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TriggerEvent {
    id: TriggerId,
    state: TriggerState,
}

struct KeyboardEventContext<'a> {
    suspended: &'a AtomicBool,
    actions: &'a RwLock<BindingActions>,
    active_hold_ids: &'a mut HashSet<TriggerId>,
    physical_release_seen: &'a mut HashMap<u32, Instant>,
    release_samples: &'a Mutex<VecDeque<f64>>,
    control: &'a EngineControl,
    ui_ctx: &'a egui::Context,
}

#[derive(Default)]
struct BindingActions {
    keyboard: HashMap<u32, HotkeyAction>,
    keyboard_codes: HashMap<u32, Code>,
    xinput: HashMap<XInputControl, HotkeyAction>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct KeyboardReleaseLatency {
    pub samples: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub assist_active: bool,
}

pub struct HotkeyRegistry {
    manager: Option<GlobalHotKeyManager>,
    registered: HashMap<u32, HotKey>,
    actions: Arc<RwLock<BindingActions>>,
    suspended: Arc<AtomicBool>,
    reset_epoch: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    control: EngineControl,
    xinput_slot: Arc<AtomicUsize>,
    xinput_down: Arc<RwLock<HashSet<XInputControl>>>,
    capture_active: Arc<AtomicBool>,
    capture_ignored: Arc<Mutex<HashSet<XInputControl>>>,
    capture_queue: Arc<Mutex<VecDeque<XInputControl>>>,
    keyboard_release_samples: Arc<Mutex<VecDeque<f64>>>,
}

impl HotkeyRegistry {
    pub fn new(control: EngineControl, ui_ctx: egui::Context) -> Self {
        let manager = GlobalHotKeyManager::new().ok();
        if manager.is_some() {
            tracing::debug!("global hotkey manager initialized");
        } else {
            tracing::warn!(
                "global hotkey manager could not be initialized; keyboard bindings unavailable"
            );
        }
        let actions = Arc::new(RwLock::new(BindingActions::default()));
        let suspended = Arc::new(AtomicBool::new(false));
        let reset_epoch = Arc::new(AtomicU64::new(0));
        let running = Arc::new(AtomicBool::new(true));
        let xinput_slot = Arc::new(AtomicUsize::new(SLOT_NONE));
        let xinput_down = Arc::new(RwLock::new(HashSet::new()));
        let capture_active = Arc::new(AtomicBool::new(false));
        let capture_ignored = Arc::new(Mutex::new(HashSet::new()));
        let capture_queue = Arc::new(Mutex::new(VecDeque::new()));
        let keyboard_release_samples =
            Arc::new(Mutex::new(VecDeque::with_capacity(RELEASE_METRIC_SAMPLES)));

        let listener_actions = Arc::clone(&actions);
        let listener_suspended = Arc::clone(&suspended);
        let listener_reset_epoch = Arc::clone(&reset_epoch);
        let listener_running = Arc::clone(&running);
        let listener_control = control.clone();
        let listener_xinput_slot = Arc::clone(&xinput_slot);
        let listener_xinput_down = Arc::clone(&xinput_down);
        let listener_capture_active = Arc::clone(&capture_active);
        let listener_capture_ignored = Arc::clone(&capture_ignored);
        let listener_capture_queue = Arc::clone(&capture_queue);
        let listener_keyboard_release_samples = Arc::clone(&keyboard_release_samples);

        let listener = thread::Builder::new()
            .name("nice-aimassist-inputs".to_owned())
            .spawn(move || {
                tracing::info!("hotkey and XInput listener started");
                let receiver = GlobalHotKeyEvent::receiver();
                let mut active_hold_ids = HashSet::new();
                let mut seen_reset_epoch = listener_reset_epoch.load(Ordering::Acquire);
                let mut xinput = XInputPoller::new();
                let mut physical_release_seen: HashMap<u32, Instant> = HashMap::new();

                while listener_running.load(Ordering::Acquire) {
                    let reset_epoch = listener_reset_epoch.load(Ordering::Acquire);
                    if reset_epoch != seen_reset_epoch {
                        seen_reset_epoch = reset_epoch;
                        active_hold_ids.clear();
                        listener_control.set_continuous(false);
                        ui_ctx.request_repaint();
                    }

                    let physical_releases = probe_keyboard_releases(
                        &active_hold_ids,
                        &listener_actions,
                        &mut physical_release_seen,
                    );
                    if release_assist_enabled(&listener_keyboard_release_samples) {
                        for id in physical_releases {
                            dispatch_trigger(
                                TriggerEvent {
                                    id: TriggerId::Keyboard(id),
                                    state: TriggerState::Released,
                                },
                                &listener_actions,
                                &mut active_hold_ids,
                                &listener_control,
                                &ui_ctx,
                            );
                        }
                    }
                    let has_keyboard_hold = active_hold_ids
                        .iter()
                        .any(|id| matches!(id, TriggerId::Keyboard(_)));
                    let now = Instant::now();
                    let mut wait = xinput.wait_duration(now).min(LISTENER_MAX_SLEEP);
                    if has_keyboard_hold {
                        wait = wait.min(KEYBOARD_RELEASE_PROBE_INTERVAL);
                    }
                    if let Ok(event) = receiver.recv_timeout(wait) {
                        process_keyboard_event(
                            event,
                            KeyboardEventContext {
                                suspended: &listener_suspended,
                                actions: &listener_actions,
                                active_hold_ids: &mut active_hold_ids,
                                physical_release_seen: &mut physical_release_seen,
                                release_samples: &listener_keyboard_release_samples,
                                control: &listener_control,
                                ui_ctx: &ui_ctx,
                            },
                        );
                        while let Ok(event) = receiver.try_recv() {
                            process_keyboard_event(
                                event,
                                KeyboardEventContext {
                                    suspended: &listener_suspended,
                                    actions: &listener_actions,
                                    active_hold_ids: &mut active_hold_ids,
                                    physical_release_seen: &mut physical_release_seen,
                                    release_samples: &listener_keyboard_release_samples,
                                    control: &listener_control,
                                    ui_ctx: &ui_ctx,
                                },
                            );
                        }
                    }

                    if let Some(edges) = xinput.poll_if_due(Instant::now()) {
                        listener_xinput_slot.store(
                            xinput.slot().map_or(SLOT_NONE, |slot| slot as usize),
                            Ordering::Release,
                        );
                        *listener_xinput_down
                            .write()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                            xinput.pressed().clone();

                        // Edges alone are not an authoritative hold state: a
                        // controller can already be held when we start polling
                        // or reconnect. Reconcile from the physical snapshot so
                        // releasing a second binding never disables the first.
                        if !listener_suspended.load(Ordering::Acquire)
                            && let Some(enabled) = reconcile_xinput_holds(
                                xinput.pressed(),
                                &listener_actions,
                                &mut active_hold_ids,
                            )
                        {
                            tracing::debug!(enabled, "continuous hotkey state reconciled");
                            listener_control.set_continuous(enabled);
                            ui_ctx.request_repaint();
                        }

                        for edge in edges {
                            let state = match edge.state {
                                EdgeState::Pressed => TriggerState::Pressed,
                                EdgeState::Released => TriggerState::Released,
                            };

                            if listener_capture_active.load(Ordering::Acquire) {
                                let should_queue = {
                                    let mut ignored = listener_capture_ignored
                                        .lock()
                                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                                    capture_accepts_event(edge.control, state, &mut ignored)
                                };
                                if should_queue {
                                    let mut queue = listener_capture_queue
                                        .lock()
                                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                                    if !queue.contains(&edge.control) {
                                        queue.push_back(edge.control);
                                    }
                                    drop(queue);
                                    ui_ctx.request_repaint();
                                }
                            }

                            if !listener_suspended.load(Ordering::Acquire) {
                                dispatch_trigger(
                                    TriggerEvent {
                                        id: TriggerId::XInput(edge.control),
                                        state,
                                    },
                                    &listener_actions,
                                    &mut active_hold_ids,
                                    &listener_control,
                                    &ui_ctx,
                                );
                            }
                        }
                    }
                }

                if !active_hold_ids.is_empty() {
                    listener_control.set_continuous(false);
                }
                tracing::info!("hotkey and XInput listener stopped");
            })
            .ok();

        Self {
            manager,
            registered: HashMap::new(),
            actions,
            suspended,
            reset_epoch,
            running,
            listener,
            control,
            xinput_slot,
            xinput_down,
            capture_active,
            capture_ignored,
            capture_queue,
            keyboard_release_samples,
        }
    }

    pub fn replace_bindings(&mut self, bindings: &[HotkeyBinding]) -> Result<(), String> {
        tracing::debug!(count = bindings.len(), "replacing hotkey bindings");
        let desired = parse_bindings(bindings)?;

        if (!desired.keyboard.is_empty() || !self.registered.is_empty()) && self.manager.is_none() {
            return Err("Global hotkey manager could not be created".to_owned());
        }

        if let Some(manager) = self.manager.as_ref() {
            let mut newly_registered = Vec::new();
            for (id, (hotkey, _action)) in &desired.keyboard {
                if self.registered.contains_key(id) {
                    continue;
                }
                if let Err(error) = manager.register(*hotkey) {
                    for rollback in newly_registered {
                        let _ = manager.unregister(rollback);
                    }
                    return Err(format!(
                        "Failed to register {}: {error}",
                        display_hotkey(*hotkey)
                    ));
                }
                newly_registered.push(*hotkey);
            }

            let removed: Vec<HotKey> = self
                .registered
                .iter()
                .filter(|(id, _)| !desired.keyboard.contains_key(id))
                .map(|(_, hotkey)| *hotkey)
                .collect();
            for hotkey in removed {
                let _ = manager.unregister(hotkey);
            }
        }

        let xinput_count = desired.xinput.len();
        self.registered = desired
            .keyboard
            .iter()
            .map(|(id, (hotkey, _))| (*id, *hotkey))
            .collect();
        *self
            .actions
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = BindingActions {
            keyboard: desired
                .keyboard
                .iter()
                .map(|(id, (_, action))| (*id, *action))
                .collect(),
            keyboard_codes: desired
                .keyboard
                .iter()
                .map(|(id, (hotkey, _))| (*id, hotkey.key))
                .collect(),
            xinput: desired.xinput,
        };
        tracing::debug!(
            keyboard = self.registered.len(),
            xinput = xinput_count,
            "hotkey bindings registered"
        );
        Ok(())
    }

    pub fn suspend(&self) {
        tracing::debug!("hotkey dispatch suspended");
        self.suspended.store(true, Ordering::Release);
        self.reset_epoch.fetch_add(1, Ordering::AcqRel);
        self.control.set_continuous(false);
    }

    pub fn resume(&self) {
        tracing::debug!("hotkey dispatch resumed");
        self.suspended.store(false, Ordering::Release);
    }

    pub fn begin_capture(&self) {
        tracing::debug!("hotkey capture enabled");
        self.suspend();
        self.capture_active.store(false, Ordering::Release);
        self.capture_queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        *self
            .capture_ignored
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = self
            .xinput_down
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        self.capture_active.store(true, Ordering::Release);
    }

    pub fn end_capture(&self) {
        tracing::debug!("hotkey capture disabled");
        self.capture_active.store(false, Ordering::Release);
        self.capture_queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.capture_ignored
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.resume();
    }

    pub fn poll_captured_xinput(&self) -> Option<String> {
        self.capture_queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .map(XInputControl::shortcut)
    }

    pub fn xinput_slot(&self) -> Option<u32> {
        match self.xinput_slot.load(Ordering::Acquire) {
            SLOT_NONE => None,
            slot => Some(slot as u32),
        }
    }

    pub fn keyboard_release_latency(&self) -> KeyboardReleaseLatency {
        let samples = self
            .keyboard_release_samples
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        summarize_release_latency(&samples)
    }
}

impl Drop for HotkeyRegistry {
    fn drop(&mut self) {
        tracing::debug!("shutting down hotkey registry");
        self.capture_active.store(false, Ordering::Release);
        self.suspended.store(true, Ordering::Release);
        self.running.store(false, Ordering::Release);
        self.control.set_continuous(false);

        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        if let Some(manager) = &self.manager {
            for hotkey in self.registered.values().copied() {
                let _ = manager.unregister(hotkey);
            }
        }
    }
}

fn process_keyboard_event(event: GlobalHotKeyEvent, context: KeyboardEventContext<'_>) {
    if context.suspended.load(Ordering::Acquire) {
        return;
    }
    let state = match event.state {
        HotKeyState::Pressed => {
            context.physical_release_seen.remove(&event.id);
            TriggerState::Pressed
        }
        HotKeyState::Released => {
            if let Some(observed) = context.physical_release_seen.remove(&event.id) {
                let latency_ms = observed.elapsed().as_secs_f64() * 1000.0;
                let mut samples = context
                    .release_samples
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if samples.len() == RELEASE_METRIC_SAMPLES {
                    samples.pop_front();
                }
                samples.push_back(latency_ms);
            }
            TriggerState::Released
        }
    };
    tracing::debug!(id = event.id, ?state, "global hotkey event received");
    dispatch_trigger(
        TriggerEvent {
            id: TriggerId::Keyboard(event.id),
            state,
        },
        context.actions,
        context.active_hold_ids,
        context.control,
        context.ui_ctx,
    );
}

fn probe_keyboard_releases(
    active: &HashSet<TriggerId>,
    actions: &RwLock<BindingActions>,
    seen: &mut HashMap<u32, Instant>,
) -> Vec<u32> {
    let actions = actions
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let now = Instant::now();
    let mut released = Vec::new();
    for id in active {
        let TriggerId::Keyboard(id) = *id else {
            continue;
        };
        if seen.contains_key(&id) {
            continue;
        }
        let Some(code) = actions.keyboard_codes.get(&id).copied() else {
            continue;
        };
        let Some(vkey) = code_to_vkey(code) else {
            continue;
        };
        // SAFETY: GetAsyncKeyState accepts any virtual-key code and has no pointer arguments.
        let down = unsafe { GetAsyncKeyState(vkey) } as u16 & 0x8000 != 0;
        if !down {
            seen.insert(id, now);
            released.push(id);
        }
    }
    seen.retain(|_, observed| now.duration_since(*observed) <= RELEASE_OBSERVATION_TTL);
    released
}

fn release_assist_enabled(samples: &Mutex<VecDeque<f64>>) -> bool {
    let samples = samples
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let summary = summarize_release_latency(&samples);
    summary.samples >= RELEASE_ASSIST_MIN_SAMPLES
        && summary.p95_ms > RELEASE_ASSIST_P95_THRESHOLD_MS
}

fn code_to_vkey(code: Code) -> Option<i32> {
    Some(match code {
        Code::KeyA => 0x41,
        Code::KeyB => 0x42,
        Code::KeyC => 0x43,
        Code::KeyD => 0x44,
        Code::KeyE => 0x45,
        Code::KeyF => 0x46,
        Code::KeyG => 0x47,
        Code::KeyH => 0x48,
        Code::KeyI => 0x49,
        Code::KeyJ => 0x4A,
        Code::KeyK => 0x4B,
        Code::KeyL => 0x4C,
        Code::KeyM => 0x4D,
        Code::KeyN => 0x4E,
        Code::KeyO => 0x4F,
        Code::KeyP => 0x50,
        Code::KeyQ => 0x51,
        Code::KeyR => 0x52,
        Code::KeyS => 0x53,
        Code::KeyT => 0x54,
        Code::KeyU => 0x55,
        Code::KeyV => 0x56,
        Code::KeyW => 0x57,
        Code::KeyX => 0x58,
        Code::KeyY => 0x59,
        Code::KeyZ => 0x5A,
        Code::Digit0 => 0x30,
        Code::Digit1 => 0x31,
        Code::Digit2 => 0x32,
        Code::Digit3 => 0x33,
        Code::Digit4 => 0x34,
        Code::Digit5 => 0x35,
        Code::Digit6 => 0x36,
        Code::Digit7 => 0x37,
        Code::Digit8 => 0x38,
        Code::Digit9 => 0x39,
        Code::F1 => 0x70,
        Code::F2 => 0x71,
        Code::F3 => 0x72,
        Code::F4 => 0x73,
        Code::F5 => 0x74,
        Code::F6 => 0x75,
        Code::F7 => 0x76,
        Code::F8 => 0x77,
        Code::F9 => 0x78,
        Code::F10 => 0x79,
        Code::F11 => 0x7A,
        Code::F12 => 0x7B,
        Code::F13 => 0x7C,
        Code::F14 => 0x7D,
        Code::F15 => 0x7E,
        Code::F16 => 0x7F,
        Code::F17 => 0x80,
        Code::F18 => 0x81,
        Code::F19 => 0x82,
        Code::F20 => 0x83,
        Code::F21 => 0x84,
        Code::F22 => 0x85,
        Code::F23 => 0x86,
        Code::F24 => 0x87,
        Code::ArrowLeft => 0x25,
        Code::ArrowUp => 0x26,
        Code::ArrowRight => 0x27,
        Code::ArrowDown => 0x28,
        Code::Backspace => 0x08,
        Code::Enter => 0x0D,
        Code::Space => 0x20,
        Code::Tab => 0x09,
        Code::Insert => 0x2D,
        Code::Delete => 0x2E,
        Code::Home => 0x24,
        Code::End => 0x23,
        Code::PageUp => 0x21,
        Code::PageDown => 0x22,
        Code::BracketLeft => 0xDB,
        Code::BracketRight => 0xDD,
        Code::Backquote => 0xC0,
        Code::Equal => 0xBB,
        Code::Comma => 0xBC,
        Code::Backslash => 0xDC,
        Code::Slash => 0xBF,
        Code::Minus => 0xBD,
        Code::Period => 0xBE,
        Code::Semicolon => 0xBA,
        Code::Quote => 0xDE,
        Code::BrowserBack => 0xA6,
        _ => return None,
    })
}

fn summarize_release_latency(samples: &VecDeque<f64>) -> KeyboardReleaseLatency {
    if samples.is_empty() {
        return KeyboardReleaseLatency::default();
    }
    let mut sorted = samples.iter().copied().collect::<Vec<_>>();
    sorted.sort_by(f64::total_cmp);
    let percentile = |fraction: f64| {
        let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
        sorted[index]
    };
    let p95_ms = percentile(0.95);
    KeyboardReleaseLatency {
        samples: sorted.len(),
        p50_ms: percentile(0.50),
        p95_ms,
        assist_active: sorted.len() >= RELEASE_ASSIST_MIN_SAMPLES
            && p95_ms > RELEASE_ASSIST_P95_THRESHOLD_MS,
    }
}

fn dispatch_trigger(
    event: TriggerEvent,
    actions: &RwLock<BindingActions>,
    active_hold_ids: &mut HashSet<TriggerId>,
    control: &EngineControl,
    ui_ctx: &egui::Context,
) {
    let action = {
        let actions = actions
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match event.id {
            TriggerId::Keyboard(id) => actions.keyboard.get(&id).copied(),
            TriggerId::XInput(input) => actions.xinput.get(&input).copied(),
        }
    };

    match action {
        Some(HotkeyAction::ContinuousHold) => {
            if let Some(enabled) = update_hold_state(active_hold_ids, event) {
                tracing::debug!(enabled, "continuous hotkey state changed");
                control.set_continuous(enabled);
            }
            ui_ctx.request_repaint();
        }
        Some(HotkeyAction::RunOnce) if event.state == TriggerState::Pressed => {
            tracing::debug!("run-once hotkey triggered");
            control.run_once();
            ui_ctx.request_repaint();
        }
        _ => {}
    }
}

fn update_hold_state(active: &mut HashSet<TriggerId>, event: TriggerEvent) -> Option<bool> {
    match event.state {
        TriggerState::Pressed => {
            let was_empty = active.is_empty();
            let inserted = active.insert(event.id);
            (inserted && was_empty).then_some(true)
        }
        TriggerState::Released => {
            let removed = active.remove(&event.id);
            (removed && active.is_empty()).then_some(false)
        }
    }
}

fn reconcile_xinput_holds(
    pressed: &HashSet<XInputControl>,
    actions: &RwLock<BindingActions>,
    active: &mut HashSet<TriggerId>,
) -> Option<bool> {
    let was_enabled = !active.is_empty();
    let actions = actions
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let desired: HashSet<_> = pressed
        .iter()
        .filter(|control| actions.xinput.get(control) == Some(&HotkeyAction::ContinuousHold))
        .map(|control| TriggerId::XInput(*control))
        .collect();
    active.retain(|id| !matches!(id, TriggerId::XInput(_)) || desired.contains(id));
    active.extend(desired);
    let enabled = !active.is_empty();
    (enabled != was_enabled).then_some(enabled)
}

fn capture_accepts_event(
    control: XInputControl,
    state: TriggerState,
    ignored: &mut HashSet<XInputControl>,
) -> bool {
    match state {
        TriggerState::Pressed => !ignored.contains(&control),
        TriggerState::Released => {
            ignored.remove(&control);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiple_holds_only_stop_after_last_release() {
        let a = TriggerId::Keyboard(1);
        let b = TriggerId::XInput(XInputControl::A);
        let mut active = HashSet::new();
        assert_eq!(
            update_hold_state(
                &mut active,
                TriggerEvent {
                    id: a,
                    state: TriggerState::Pressed,
                }
            ),
            Some(true)
        );
        assert_eq!(
            update_hold_state(
                &mut active,
                TriggerEvent {
                    id: b,
                    state: TriggerState::Pressed,
                }
            ),
            None
        );
        assert_eq!(
            update_hold_state(
                &mut active,
                TriggerEvent {
                    id: a,
                    state: TriggerState::Released,
                }
            ),
            None
        );
        assert_eq!(
            update_hold_state(
                &mut active,
                TriggerEvent {
                    id: b,
                    state: TriggerState::Released,
                }
            ),
            Some(false)
        );
    }

    #[test]
    fn preheld_xinput_hold_is_reconciled_with_other_holds() {
        let mut actions = BindingActions::default();
        actions
            .xinput
            .insert(XInputControl::A, HotkeyAction::ContinuousHold);
        let actions = RwLock::new(actions);
        let mut active = HashSet::from([TriggerId::Keyboard(1)]);
        assert_eq!(
            reconcile_xinput_holds(&HashSet::from([XInputControl::A]), &actions, &mut active),
            None
        );
        assert!(active.contains(&TriggerId::XInput(XInputControl::A)));
        assert_eq!(
            update_hold_state(
                &mut active,
                TriggerEvent {
                    id: TriggerId::Keyboard(1),
                    state: TriggerState::Released
                }
            ),
            None
        );
        assert!(!active.is_empty());
    }

    #[test]
    fn keyboard_release_assist_requires_enough_slow_samples() {
        let fast = VecDeque::from(vec![10.0; RELEASE_ASSIST_MIN_SAMPLES]);
        let fast_summary = summarize_release_latency(&fast);
        assert!(!fast_summary.assist_active);

        let slow = VecDeque::from(vec![30.0; RELEASE_ASSIST_MIN_SAMPLES]);
        let slow_summary = summarize_release_latency(&slow);
        assert!(slow_summary.assist_active);
    }

    #[test]
    fn capture_ignores_preheld_input_until_release_and_repress() {
        let mut ignored = HashSet::from([XInputControl::A]);
        assert!(!capture_accepts_event(
            XInputControl::A,
            TriggerState::Pressed,
            &mut ignored,
        ));
        assert!(!capture_accepts_event(
            XInputControl::A,
            TriggerState::Released,
            &mut ignored,
        ));
        assert!(capture_accepts_event(
            XInputControl::A,
            TriggerState::Pressed,
            &mut ignored,
        ));
    }
}
