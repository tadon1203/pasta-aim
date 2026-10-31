//! The input thread. It turns hotkey events and XInput polls into commands for the engine.
//! It sleeps on a `DeadlineWait` until the UI sends a message or the next poll or release probe is due.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crossbeam_channel::Receiver;
use global_hotkey::{GlobalHotKeyEvent, HotKeyState, hotkey::Code};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

use super::{BindingActions, InputView, KeyboardReleaseLatency, ListenerMessage};
use crate::{
    engine::{Command, EngineControl},
    repaint::RepaintSignal,
    settings::HotkeyAction,
    sync::MutexExt,
    wait::DeadlineWait,
};

use super::xinput::{ControlSet, EdgeState, Poller, XInputControl};

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

/// The state of the input thread. Only the thread touches it; the UI reads `InputView`.
pub(super) struct Listener {
    rx: Receiver<ListenerMessage>,
    wait: DeadlineWait,
    control: EngineControl,
    repaint: RepaintSignal,
    view: Arc<Mutex<InputView>>,
    bindings: BindingActions,
    suspended: bool,
    xinput: Poller,
    /// Some while a recording is active. It holds the XInput controls that were down at its start.
    /// They are ignored until they are released.
    capture: Option<ControlSet>,
    published_slot: Option<u32>,
    active_hold_ids: HashSet<TriggerId>,
    physical_release_seen: HashMap<u32, Instant>,
    next_release_probe: Instant,
    release_samples: VecDeque<f64>,
    release_summary: KeyboardReleaseLatency,
}

impl Listener {
    pub(super) fn new(
        rx: Receiver<ListenerMessage>,
        wait: DeadlineWait,
        control: EngineControl,
        repaint: RepaintSignal,
        view: Arc<Mutex<InputView>>,
    ) -> Self {
        Self {
            rx,
            wait,
            control,
            repaint,
            view,
            bindings: BindingActions::default(),
            suspended: false,
            xinput: Poller::new(),
            capture: None,
            published_slot: None,
            active_hold_ids: HashSet::new(),
            physical_release_seen: HashMap::new(),
            next_release_probe: Instant::now(),
            release_samples: VecDeque::with_capacity(RELEASE_METRIC_SAMPLES),
            release_summary: KeyboardReleaseLatency::default(),
        }
    }

    pub(super) fn run(mut self) {
        tracing::info!("hotkey and XInput listener started");
        loop {
            let deadline = self.next_deadline();
            self.wait.wait_until(Some(deadline), Duration::ZERO);
            if !self.drain() {
                break;
            }
            let now = Instant::now();
            self.xinput.set_fast(self.wants_fast(), now);
            self.probe_releases_if_due(now);
            self.poll_xinput(now);
        }

        if !self.active_hold_ids.is_empty() {
            self.control.send(Command::SetContinuous(false));
        }
        tracing::info!("hotkey and XInput listener stopped");
    }

    /// Handles every queued message in the order it was sent. Returns `false` on `Shutdown`.
    fn drain(&mut self) -> bool {
        while let Ok(message) = self.rx.try_recv() {
            if !self.handle(message) {
                return false;
            }
        }
        true
    }

    fn handle(&mut self, message: ListenerMessage) -> bool {
        match message {
            ListenerMessage::Key(event) => self.on_key(event),
            ListenerMessage::Bindings(actions) => self.bindings = actions,
            ListenerMessage::Suspend => {
                self.suspended = true;
                self.active_hold_ids.clear();
                self.control.send(Command::SetContinuous(false));
                self.repaint.request();
            }
            ListenerMessage::Resume => self.suspended = false,
            ListenerMessage::BeginCapture => {
                // Read the controller now. A snapshot from a slow poll can be a second old, and the controls
                // that were pressed since then must count as down. Their edges are dropped: dispatch is suspended.
                self.xinput.poll_now(Instant::now());
                self.capture = Some(self.xinput.pressed());
                self.update_view(|view| view.captured.clear());
            }
            ListenerMessage::EndCapture => {
                self.capture = None;
                self.update_view(|view| view.captured.clear());
            }
            ListenerMessage::Shutdown => return false,
        }
        true
    }

    /// The earlier of the next XInput poll and the next key release probe.
    fn next_deadline(&self) -> Instant {
        let poll = self.xinput.next_poll();
        if self.has_keyboard_hold() {
            poll.min(self.next_release_probe)
        } else {
            poll
        }
    }

    fn has_keyboard_hold(&self) -> bool {
        self.active_hold_ids
            .iter()
            .any(|id| matches!(id, TriggerId::Keyboard(_)))
    }

    /// XInput is polled at the fast rate while a bound XInput Shortcut or a recording needs it.
    fn wants_fast(&self) -> bool {
        !self.bindings.xinput.is_empty() || self.capture.is_some()
    }

    fn on_key(&mut self, event: GlobalHotKeyEvent) {
        if self.suspended {
            return;
        }
        let state = match event.state {
            HotKeyState::Pressed => {
                self.physical_release_seen.remove(&event.id);
                TriggerState::Pressed
            }
            HotKeyState::Released => {
                if let Some(observed) = self.physical_release_seen.remove(&event.id) {
                    self.record_release(observed);
                }
                TriggerState::Released
            }
        };
        tracing::debug!(id = event.id, ?state, "global hotkey event received");
        self.dispatch(TriggerEvent {
            id: TriggerId::Keyboard(event.id),
            state,
        });
    }

    /// Adds one release latency. The summary is made here, once per sample.
    fn record_release(&mut self, observed: Instant) {
        if self.release_samples.len() == RELEASE_METRIC_SAMPLES {
            self.release_samples.pop_front();
        }
        self.release_samples
            .push_back(observed.elapsed().as_secs_f64() * 1000.0);
        let summary = summarize_release_latency(&self.release_samples);
        if summary != self.release_summary {
            self.release_summary = summary;
            self.update_view(move |view| view.release_latency = summary);
        }
    }

    /// Looks for held keyboard Hold shortcuts that were released without a key event.
    fn probe_releases_if_due(&mut self, now: Instant) {
        if !self.has_keyboard_hold() || now < self.next_release_probe {
            return;
        }
        self.next_release_probe = now + KEYBOARD_RELEASE_PROBE_INTERVAL;
        let released = probe_keyboard_releases(
            &self.active_hold_ids,
            &self.bindings,
            &mut self.physical_release_seen,
            now,
        );
        if self.release_summary.assist_active {
            for id in released {
                self.dispatch(TriggerEvent {
                    id: TriggerId::Keyboard(id),
                    state: TriggerState::Released,
                });
            }
        }
    }

    fn poll_xinput(&mut self, now: Instant) {
        let Some(edges) = self.xinput.poll_if_due(now) else {
            return;
        };

        let slot = self.xinput.slot();
        if slot != self.published_slot {
            self.published_slot = slot;
            self.update_view(move |view| view.xinput_slot = slot);
        }

        // Edges alone are not an authoritative hold state: a controller can already be held
        // when we start polling or reconnect. Reconcile from the physical snapshot so
        // releasing a second binding never disables the first.
        let pressed = self.xinput.pressed();
        if !self.suspended
            && let Some(enabled) =
                reconcile_xinput_holds(pressed, &self.bindings, &mut self.active_hold_ids)
        {
            tracing::debug!(enabled, "continuous hotkey state reconciled");
            self.control.send(Command::SetContinuous(enabled));
            self.repaint.request();
        }

        for edge in edges {
            let state = match edge.state {
                EdgeState::Pressed => TriggerState::Pressed,
                EdgeState::Released => TriggerState::Released,
            };
            let control = edge.control;
            let recorded = match self.capture.as_mut() {
                Some(ignored) => capture_accepts_event(control, state, ignored),
                None => false,
            };
            if recorded {
                self.update_view(move |view| {
                    if !view.captured.contains(&control) {
                        view.captured.push_back(control);
                    }
                });
            }

            if !self.suspended {
                self.dispatch(TriggerEvent {
                    id: TriggerId::XInput(control),
                    state,
                });
            }
        }
    }

    fn dispatch(&mut self, event: TriggerEvent) {
        let action = match event.id {
            TriggerId::Keyboard(id) => self.bindings.keyboard.get(&id).copied(),
            TriggerId::XInput(input) => self.bindings.xinput.get(&input).copied(),
        };

        match action {
            Some(HotkeyAction::ContinuousHold) => {
                if let Some(enabled) = update_hold_state(&mut self.active_hold_ids, event) {
                    tracing::debug!(enabled, "continuous hotkey state changed");
                    self.control.send(Command::SetContinuous(enabled));
                }
                self.repaint.request();
            }
            Some(HotkeyAction::RunOnce) if event.state == TriggerState::Pressed => {
                tracing::debug!("run-once hotkey triggered");
                self.control.send(Command::RunOnce);
                self.repaint.request();
            }
            _ => {}
        }
    }

    /// Changes the view for the UI and asks for a repaint. Call it only when a value changed.
    fn update_view(&self, update: impl FnOnce(&mut InputView)) {
        update(&mut self.view.lock_recover());
        self.repaint.request();
    }
}

fn probe_keyboard_releases(
    active: &HashSet<TriggerId>,
    bindings: &BindingActions,
    seen: &mut HashMap<u32, Instant>,
    now: Instant,
) -> Vec<u32> {
    let mut released = Vec::new();
    for id in active {
        let TriggerId::Keyboard(id) = *id else {
            continue;
        };
        if seen.contains_key(&id) {
            continue;
        }
        let Some(code) = bindings.keyboard_codes.get(&id).copied() else {
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
    pressed: ControlSet,
    bindings: &BindingActions,
    active: &mut HashSet<TriggerId>,
) -> Option<bool> {
    let was_enabled = !active.is_empty();
    let is_hold = |control: XInputControl| {
        bindings.xinput.get(&control) == Some(&HotkeyAction::ContinuousHold)
    };
    active.retain(|id| match id {
        TriggerId::XInput(control) => pressed.contains(*control) && is_hold(*control),
        TriggerId::Keyboard(_) => true,
    });
    active.extend(
        pressed
            .iter()
            .filter(|control| is_hold(*control))
            .map(TriggerId::XInput),
    );
    let enabled = !active.is_empty();
    (enabled != was_enabled).then_some(enabled)
}

/// Returns `true` when the event is recorded. A control that was ignored is accepted again after its release.
fn capture_accepts_event(
    control: XInputControl,
    state: TriggerState,
    ignored: &mut ControlSet,
) -> bool {
    match state {
        TriggerState::Pressed => !ignored.contains(control),
        TriggerState::Released => {
            ignored.remove(control);
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
        let mut pressed = ControlSet::default();
        pressed.insert(XInputControl::A);
        let mut active = HashSet::from([TriggerId::Keyboard(1)]);
        assert_eq!(reconcile_xinput_holds(pressed, &actions, &mut active), None);
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
        let mut ignored = ControlSet::default();
        ignored.insert(XInputControl::A);
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

    #[test]
    fn suspend_releases_every_hold_and_sends_one_stop() {
        let (control, commands) = EngineControl::detached();
        let (_sender, rx) = crossbeam_channel::unbounded();
        let (wait, _waker) = DeadlineWait::new().unwrap();
        let mut listener = Listener::new(
            rx,
            wait,
            control,
            RepaintSignal::detached(),
            Arc::new(Mutex::new(InputView::default())),
        );
        listener.active_hold_ids.insert(TriggerId::Keyboard(1));
        listener
            .active_hold_ids
            .insert(TriggerId::XInput(XInputControl::A));

        listener.handle(ListenerMessage::Suspend);

        assert!(listener.active_hold_ids.is_empty());
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::SetContinuous(false))
        ));
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn fast_xinput_polling_is_wanted_only_with_a_bound_xinput_shortcut_or_a_recording() {
        let (control, _commands) = EngineControl::detached();
        let (_sender, rx) = crossbeam_channel::unbounded();
        let (wait, _waker) = DeadlineWait::new().unwrap();
        let mut listener = Listener::new(
            rx,
            wait,
            control,
            RepaintSignal::detached(),
            Arc::new(Mutex::new(InputView::default())),
        );
        assert!(!listener.wants_fast());

        listener.bindings.keyboard.insert(1, HotkeyAction::RunOnce);
        assert!(!listener.wants_fast());

        listener
            .bindings
            .xinput
            .insert(XInputControl::A, HotkeyAction::RunOnce);
        assert!(listener.wants_fast());

        listener.bindings.xinput.clear();
        listener.capture = Some(ControlSet::default());
        assert!(listener.wants_fast());
    }
}
