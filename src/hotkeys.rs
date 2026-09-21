use std::{
    collections::{HashMap, HashSet, VecDeque},
    str::FromStr,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use eframe::egui;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use windows_sys::Win32::UI::Input::XboxController::{
    XINPUT_GAMEPAD, XINPUT_GAMEPAD_A, XINPUT_GAMEPAD_B, XINPUT_GAMEPAD_BACK,
    XINPUT_GAMEPAD_DPAD_DOWN, XINPUT_GAMEPAD_DPAD_LEFT, XINPUT_GAMEPAD_DPAD_RIGHT,
    XINPUT_GAMEPAD_DPAD_UP, XINPUT_GAMEPAD_LEFT_SHOULDER, XINPUT_GAMEPAD_LEFT_THUMB,
    XINPUT_GAMEPAD_RIGHT_SHOULDER, XINPUT_GAMEPAD_RIGHT_THUMB, XINPUT_GAMEPAD_START,
    XINPUT_GAMEPAD_X, XINPUT_GAMEPAD_Y, XINPUT_STATE, XInputGetState,
};

use crate::{
    config::{HotkeyAction, HotkeyBinding},
    engine::EngineControl,
};

const XINPUT_POLL_INTERVAL: Duration = Duration::from_millis(5);
const XINPUT_SLOT_NONE: usize = usize::MAX;
const XINPUT_SLOT_COUNT: u32 = 4;
const TRIGGER_PRESS_THRESHOLD: u8 = 128;
const TRIGGER_RELEASE_THRESHOLD: u8 = 102;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureResult {
    Cancelled,
    Captured(HotKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XInputControl {
    A,
    B,
    X,
    Y,
    LeftShoulder,
    RightShoulder,
    Back,
    Start,
    LeftThumb,
    RightThumb,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    LeftTrigger,
    RightTrigger,
}

impl XInputControl {
    const ALL: [Self; 16] = [
        Self::A,
        Self::B,
        Self::X,
        Self::Y,
        Self::LeftShoulder,
        Self::RightShoulder,
        Self::Back,
        Self::Start,
        Self::LeftThumb,
        Self::RightThumb,
        Self::DpadUp,
        Self::DpadDown,
        Self::DpadLeft,
        Self::DpadRight,
        Self::LeftTrigger,
        Self::RightTrigger,
    ];

    fn slug(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
            Self::X => "x",
            Self::Y => "y",
            Self::LeftShoulder => "lb",
            Self::RightShoulder => "rb",
            Self::Back => "back",
            Self::Start => "start",
            Self::LeftThumb => "left_stick",
            Self::RightThumb => "right_stick",
            Self::DpadUp => "dpad_up",
            Self::DpadDown => "dpad_down",
            Self::DpadLeft => "dpad_left",
            Self::DpadRight => "dpad_right",
            Self::LeftTrigger => "lt",
            Self::RightTrigger => "rt",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::A => "XInput A",
            Self::B => "XInput B",
            Self::X => "XInput X",
            Self::Y => "XInput Y",
            Self::LeftShoulder => "XInput LB",
            Self::RightShoulder => "XInput RB",
            Self::Back => "XInput Back",
            Self::Start => "XInput Start",
            Self::LeftThumb => "XInput Left Stick",
            Self::RightThumb => "XInput Right Stick",
            Self::DpadUp => "XInput D-pad Up",
            Self::DpadDown => "XInput D-pad Down",
            Self::DpadLeft => "XInput D-pad Left",
            Self::DpadRight => "XInput D-pad Right",
            Self::LeftTrigger => "XInput LT",
            Self::RightTrigger => "XInput RT",
        }
    }

    fn shortcut(self) -> String {
        format!("xinput:{}", self.slug())
    }

    fn from_slug(slug: &str) -> Option<Self> {
        Some(match slug {
            "a" => Self::A,
            "b" => Self::B,
            "x" => Self::X,
            "y" => Self::Y,
            "lb" => Self::LeftShoulder,
            "rb" => Self::RightShoulder,
            "back" => Self::Back,
            "start" => Self::Start,
            "left_stick" => Self::LeftThumb,
            "right_stick" => Self::RightThumb,
            "dpad_up" => Self::DpadUp,
            "dpad_down" => Self::DpadDown,
            "dpad_left" => Self::DpadLeft,
            "dpad_right" => Self::DpadRight,
            "lt" => Self::LeftTrigger,
            "rt" => Self::RightTrigger,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct XInputSample {
    buttons: u16,
    left_trigger: u8,
    right_trigger: u8,
}

impl From<XINPUT_GAMEPAD> for XInputSample {
    fn from(gamepad: XINPUT_GAMEPAD) -> Self {
        Self {
            buttons: gamepad.wButtons,
            left_trigger: gamepad.bLeftTrigger,
            right_trigger: gamepad.bRightTrigger,
        }
    }
}

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

#[derive(Default)]
struct BindingActions {
    keyboard: HashMap<u32, HotkeyAction>,
    xinput: HashMap<XInputControl, HotkeyAction>,
}

#[derive(Default)]
struct ParsedBindings {
    keyboard: HashMap<u32, (HotKey, HotkeyAction)>,
    xinput: HashMap<XInputControl, HotkeyAction>,
}

#[derive(Default)]
struct XInputTracker {
    slot: Option<u32>,
    pressed: HashSet<XInputControl>,
}

impl XInputTracker {
    fn update(&mut self, slot: Option<u32>, sample: Option<XInputSample>) -> Vec<TriggerEvent> {
        if slot != self.slot {
            let events = self.release_all();
            self.slot = slot;
            if let (Some(_), Some(sample)) = (slot, sample) {
                self.pressed = baseline_pressed_controls(sample);
            }
            return events;
        }

        let Some(sample) = sample else {
            self.slot = None;
            return self.release_all();
        };

        let mut events = Vec::new();
        for control in XInputControl::ALL {
            let was_pressed = self.pressed.contains(&control);
            let is_pressed = control_is_pressed(sample, control, was_pressed);
            match (was_pressed, is_pressed) {
                (false, true) => {
                    self.pressed.insert(control);
                    events.push(TriggerEvent {
                        id: TriggerId::XInput(control),
                        state: TriggerState::Pressed,
                    });
                }
                (true, false) => {
                    self.pressed.remove(&control);
                    events.push(TriggerEvent {
                        id: TriggerId::XInput(control),
                        state: TriggerState::Released,
                    });
                }
                _ => {}
            }
        }
        events
    }

    fn release_all(&mut self) -> Vec<TriggerEvent> {
        self.pressed
            .drain()
            .map(|control| TriggerEvent {
                id: TriggerId::XInput(control),
                state: TriggerState::Released,
            })
            .collect()
    }
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
}

impl HotkeyRegistry {
    pub fn new(control: EngineControl, ui_ctx: egui::Context) -> Self {
        let manager = GlobalHotKeyManager::new().ok();
        let actions = Arc::new(RwLock::new(BindingActions::default()));
        let suspended = Arc::new(AtomicBool::new(false));
        let reset_epoch = Arc::new(AtomicU64::new(0));
        let running = Arc::new(AtomicBool::new(true));
        let xinput_slot = Arc::new(AtomicUsize::new(XINPUT_SLOT_NONE));
        let xinput_down = Arc::new(RwLock::new(HashSet::new()));
        let capture_active = Arc::new(AtomicBool::new(false));
        let capture_ignored = Arc::new(Mutex::new(HashSet::new()));
        let capture_queue = Arc::new(Mutex::new(VecDeque::new()));

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
        let listener = thread::Builder::new()
            .name("nice-aimassist-inputs".to_owned())
            .spawn(move || {
                let mut active_hold_ids = HashSet::new();
                let mut seen_reset_epoch = listener_reset_epoch.load(Ordering::Acquire);
                let mut xinput_tracker = XInputTracker::default();
                let mut next_xinput_poll = Instant::now();

                while listener_running.load(Ordering::Acquire) {
                    let reset_epoch = listener_reset_epoch.load(Ordering::Acquire);
                    if reset_epoch != seen_reset_epoch {
                        seen_reset_epoch = reset_epoch;
                        active_hold_ids.clear();
                        listener_control.set_continuous(false);
                        ui_ctx.request_repaint();
                    }

                    while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
                        if listener_suspended.load(Ordering::Acquire) {
                            continue;
                        }
                        let state = match event.state {
                            HotKeyState::Pressed => TriggerState::Pressed,
                            HotKeyState::Released => TriggerState::Released,
                        };
                        dispatch_trigger(
                            TriggerEvent {
                                id: TriggerId::Keyboard(event.id),
                                state,
                            },
                            &listener_actions,
                            &mut active_hold_ids,
                            &listener_control,
                            &ui_ctx,
                        );
                    }

                    let now = Instant::now();
                    if now >= next_xinput_poll {
                        next_xinput_poll = now + XINPUT_POLL_INTERVAL;
                        let current_slot = match listener_xinput_slot.load(Ordering::Acquire) {
                            XINPUT_SLOT_NONE => None,
                            slot => Some(slot as u32),
                        };
                        let selected = poll_selected_xinput(current_slot);
                        let (slot, sample) = match selected {
                            Some((slot, sample)) => (Some(slot), Some(sample)),
                            None => (None, None),
                        };
                        listener_xinput_slot.store(
                            slot.map_or(XINPUT_SLOT_NONE, |slot| slot as usize),
                            Ordering::Release,
                        );

                        let events = xinput_tracker.update(slot, sample);
                        *listener_xinput_down
                            .write()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                            xinput_tracker.pressed.clone();

                        for event in events {
                            if let TriggerId::XInput(control) = event.id
                                && listener_capture_active.load(Ordering::Acquire)
                            {
                                let should_queue = {
                                    let mut ignored = listener_capture_ignored
                                        .lock()
                                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                                    capture_accepts_event(control, event.state, &mut ignored)
                                };
                                if should_queue {
                                    let mut queue = listener_capture_queue
                                        .lock()
                                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                                    if !queue.contains(&control) {
                                        queue.push_back(control);
                                    }
                                    drop(queue);
                                    ui_ctx.request_repaint();
                                }
                            }

                            if !listener_suspended.load(Ordering::Acquire) {
                                dispatch_trigger(
                                    event,
                                    &listener_actions,
                                    &mut active_hold_ids,
                                    &listener_control,
                                    &ui_ctx,
                                );
                            }
                        }
                    }

                    thread::sleep(Duration::from_millis(1));
                }

                if !active_hold_ids.is_empty() {
                    listener_control.set_continuous(false);
                }
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
        }
    }

    pub fn replace_bindings(&mut self, bindings: &[HotkeyBinding]) -> Result<(), String> {
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
                .into_iter()
                .map(|(id, (_, action))| (id, action))
                .collect(),
            xinput: desired.xinput,
        };

        Ok(())
    }

    pub fn suspend(&self) {
        self.suspended.store(true, Ordering::Release);
        self.reset_epoch.fetch_add(1, Ordering::AcqRel);
        self.control.set_continuous(false);
    }

    pub fn resume(&self) {
        self.suspended.store(false, Ordering::Release);
    }

    pub fn begin_capture(&self) {
        self.suspend();
        self.capture_active.store(false, Ordering::Release);
        self.capture_queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();

        let mut ignored = self
            .xinput_down
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let selected_slot = match self.xinput_slot.load(Ordering::Acquire) {
            XINPUT_SLOT_NONE => None,
            slot => Some(slot as u32),
        };
        if let Some((slot, sample)) = poll_selected_xinput(selected_slot)
            && selected_slot.is_none_or(|selected| selected == slot)
        {
            ignored.extend(baseline_pressed_controls(sample));
        }
        *self
            .capture_ignored
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = ignored;
        self.capture_active.store(true, Ordering::Release);
    }

    pub fn end_capture(&self) {
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
}

impl Drop for HotkeyRegistry {
    fn drop(&mut self) {
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
                control.set_continuous(enabled);
            }
            ui_ctx.request_repaint();
        }
        Some(HotkeyAction::RunOnce) if event.state == TriggerState::Pressed => {
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

pub fn capture_from_events(events: &[egui::Event]) -> Option<Result<CaptureResult, String>> {
    for event in events {
        let egui::Event::Key {
            key,
            physical_key,
            pressed,
            repeat,
            modifiers,
            ..
        } = event
        else {
            continue;
        };
        if !*pressed || *repeat {
            continue;
        }

        let key = (*physical_key).unwrap_or(*key);
        if key == egui::Key::Escape {
            return Some(Ok(CaptureResult::Cancelled));
        }
        if matches!(key, egui::Key::SuperLeft | egui::Key::SuperRight) {
            return Some(Err(
                "Windows/Super cannot be used as a captured modifier yet".to_owned(),
            ));
        }
        if is_supported_modifier_key(key) {
            continue;
        }

        return Some(hotkey_from_egui(key, *modifiers).map(CaptureResult::Captured));
    }
    None
}

pub fn canonical_shortcut(shortcut: &str) -> Result<String, String> {
    if let Some(control) = parse_xinput_shortcut(shortcut)? {
        return Ok(control.shortcut());
    }

    HotKey::from_str(shortcut)
        .map(|hotkey| hotkey.to_string())
        .map_err(|error| format!("Invalid shortcut: {error}"))
}

pub fn display_shortcut(shortcut: &str) -> String {
    match parse_xinput_shortcut(shortcut) {
        Ok(Some(control)) => control.display_name().to_owned(),
        _ => shortcut
            .replace("control+", "Ctrl+")
            .replace("shift+", "Shift+")
            .replace("alt+", "Alt+")
            .replace("super+", "Win+")
            .replace("Key", "")
            .replace("Digit", ""),
    }
}

fn parse_bindings(bindings: &[HotkeyBinding]) -> Result<ParsedBindings, String> {
    let mut parsed = ParsedBindings::default();
    for binding in bindings {
        if let Some(control) = parse_xinput_shortcut(&binding.shortcut)? {
            if let Some(existing_action) = parsed.xinput.get(&control) {
                return Err(format!(
                    "{} is already assigned to {}",
                    control.display_name(),
                    existing_action.label()
                ));
            }
            parsed.xinput.insert(control, binding.action);
            continue;
        }

        let hotkey = HotKey::from_str(&binding.shortcut)
            .map_err(|error| format!("Invalid {} shortcut: {error}", binding.action.label()))?;
        if let Some((_, existing_action)) = parsed.keyboard.get(&hotkey.id()) {
            return Err(format!(
                "{} is already assigned to {}",
                display_hotkey(hotkey),
                existing_action.label()
            ));
        }
        parsed
            .keyboard
            .insert(hotkey.id(), (hotkey, binding.action));
    }
    Ok(parsed)
}

fn parse_xinput_shortcut(shortcut: &str) -> Result<Option<XInputControl>, String> {
    let normalized = shortcut.trim().to_ascii_lowercase();
    let Some(slug) = normalized.strip_prefix("xinput:") else {
        return Ok(None);
    };
    XInputControl::from_slug(slug).map(Some).ok_or_else(|| {
        format!(
            "Unsupported XInput shortcut: {shortcut}. Sticks and unknown controller inputs are not supported."
        )
    })
}

fn hotkey_from_egui(key: egui::Key, modifiers: egui::Modifiers) -> Result<HotKey, String> {
    let key_name = global_key_name(key)
        .ok_or_else(|| format!("{} cannot be used as a global hotkey", key.name()))?;

    let mut parts = Vec::with_capacity(4);
    if modifiers.ctrl {
        parts.push("control");
    }
    if modifiers.shift {
        parts.push("shift");
    }
    if modifiers.alt {
        parts.push("alt");
    }
    parts.push(key_name);

    HotKey::from_str(&parts.join("+"))
        .map_err(|error| format!("This key combination is not supported: {error}"))
}

fn global_key_name(key: egui::Key) -> Option<&'static str> {
    use egui::Key;
    Some(match key {
        Key::OpenBracket => "[",
        Key::CloseBracket => "]",
        Key::Backtick => "`",
        Key::Equals => "=",
        Key::Num0 => "0",
        Key::Num1 => "1",
        Key::Num2 => "2",
        Key::Num3 => "3",
        Key::Num4 => "4",
        Key::Num5 => "5",
        Key::Num6 => "6",
        Key::Num7 => "7",
        Key::Num8 => "8",
        Key::Num9 => "9",
        Key::A => "A",
        Key::B => "B",
        Key::C => "C",
        Key::D => "D",
        Key::E => "E",
        Key::F => "F",
        Key::G => "G",
        Key::H => "H",
        Key::I => "I",
        Key::J => "J",
        Key::K => "K",
        Key::L => "L",
        Key::M => "M",
        Key::N => "N",
        Key::O => "O",
        Key::P => "P",
        Key::Q => "Q",
        Key::R => "R",
        Key::S => "S",
        Key::T => "T",
        Key::U => "U",
        Key::V => "V",
        Key::W => "W",
        Key::X => "X",
        Key::Y => "Y",
        Key::Z => "Z",
        Key::F1 => "F1",
        Key::F2 => "F2",
        Key::F3 => "F3",
        Key::F4 => "F4",
        Key::F5 => "F5",
        Key::F6 => "F6",
        Key::F7 => "F7",
        Key::F8 => "F8",
        Key::F9 => "F9",
        Key::F10 => "F10",
        Key::F11 => "F11",
        Key::F12 => "F12",
        Key::F13 => "F13",
        Key::F14 => "F14",
        Key::F15 => "F15",
        Key::F16 => "F16",
        Key::F17 => "F17",
        Key::F18 => "F18",
        Key::F19 => "F19",
        Key::F20 => "F20",
        Key::F21 => "F21",
        Key::F22 => "F22",
        Key::F23 => "F23",
        Key::F24 => "F24",
        Key::ArrowDown => "ArrowDown",
        Key::ArrowLeft => "ArrowLeft",
        Key::ArrowRight => "ArrowRight",
        Key::ArrowUp => "ArrowUp",
        Key::Backspace => "Backspace",
        Key::Enter => "Enter",
        Key::Space => "Space",
        Key::Tab => "Tab",
        Key::Insert => "Insert",
        Key::Delete => "Delete",
        Key::Home => "Home",
        Key::End => "End",
        Key::PageUp => "PageUp",
        Key::PageDown => "PageDown",
        Key::Comma => ",",
        Key::Backslash => "\\",
        Key::Slash => "/",
        Key::Minus => "-",
        Key::Period => ".",
        Key::Semicolon => ";",
        Key::Quote => "'",
        Key::BrowserBack => "BrowserBack",
        _ => return None,
    })
}

fn is_supported_modifier_key(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::ShiftLeft
            | egui::Key::ShiftRight
            | egui::Key::ControlLeft
            | egui::Key::ControlRight
            | egui::Key::AltLeft
            | egui::Key::AltRight
    )
}

fn display_hotkey(hotkey: HotKey) -> String {
    display_shortcut(&hotkey.to_string())
}

fn control_is_pressed(sample: XInputSample, control: XInputControl, was_pressed: bool) -> bool {
    match control {
        XInputControl::LeftTrigger => trigger_is_pressed(sample.left_trigger, was_pressed),
        XInputControl::RightTrigger => trigger_is_pressed(sample.right_trigger, was_pressed),
        _ => sample.buttons & button_mask(control) != 0,
    }
}

fn trigger_is_pressed(value: u8, was_pressed: bool) -> bool {
    if was_pressed {
        value >= TRIGGER_RELEASE_THRESHOLD
    } else {
        value >= TRIGGER_PRESS_THRESHOLD
    }
}

fn baseline_pressed_controls(sample: XInputSample) -> HashSet<XInputControl> {
    XInputControl::ALL
        .into_iter()
        .filter(|control| control_is_pressed(sample, *control, false))
        .collect()
}

fn button_mask(control: XInputControl) -> u16 {
    match control {
        XInputControl::A => XINPUT_GAMEPAD_A,
        XInputControl::B => XINPUT_GAMEPAD_B,
        XInputControl::X => XINPUT_GAMEPAD_X,
        XInputControl::Y => XINPUT_GAMEPAD_Y,
        XInputControl::LeftShoulder => XINPUT_GAMEPAD_LEFT_SHOULDER,
        XInputControl::RightShoulder => XINPUT_GAMEPAD_RIGHT_SHOULDER,
        XInputControl::Back => XINPUT_GAMEPAD_BACK,
        XInputControl::Start => XINPUT_GAMEPAD_START,
        XInputControl::LeftThumb => XINPUT_GAMEPAD_LEFT_THUMB,
        XInputControl::RightThumb => XINPUT_GAMEPAD_RIGHT_THUMB,
        XInputControl::DpadUp => XINPUT_GAMEPAD_DPAD_UP,
        XInputControl::DpadDown => XINPUT_GAMEPAD_DPAD_DOWN,
        XInputControl::DpadLeft => XINPUT_GAMEPAD_DPAD_LEFT,
        XInputControl::DpadRight => XINPUT_GAMEPAD_DPAD_RIGHT,
        XInputControl::LeftTrigger | XInputControl::RightTrigger => 0,
    }
}

fn poll_selected_xinput(current_slot: Option<u32>) -> Option<(u32, XInputSample)> {
    select_xinput_with(current_slot, read_xinput_slot)
}

fn select_xinput_with(
    current_slot: Option<u32>,
    mut read_slot: impl FnMut(u32) -> Option<XInputSample>,
) -> Option<(u32, XInputSample)> {
    if let Some(slot) = current_slot
        && let Some(sample) = read_slot(slot)
    {
        return Some((slot, sample));
    }

    (0..XINPUT_SLOT_COUNT).find_map(|slot| read_slot(slot).map(|sample| (slot, sample)))
}

fn read_xinput_slot(slot: u32) -> Option<XInputSample> {
    let mut state = XINPUT_STATE::default();
    // SAFETY: XInputGetState writes to the valid XINPUT_STATE pointer for the duration of this
    // call. `slot` is constrained to XInput's documented 0..=3 user-index range by callers.
    let result = unsafe { XInputGetState(slot, &mut state) };
    (result == 0).then(|| XInputSample::from(state.Gamepad))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(action: HotkeyAction, shortcut: &str) -> HotkeyBinding {
        HotkeyBinding {
            action,
            shortcut: shortcut.to_owned(),
        }
    }

    fn sample(buttons: u16, left_trigger: u8, right_trigger: u8) -> XInputSample {
        XInputSample {
            buttons,
            left_trigger,
            right_trigger,
        }
    }

    #[test]
    fn duplicate_keyboard_shortcuts_are_rejected_across_actions() {
        let bindings = vec![
            binding(HotkeyAction::ContinuousHold, "F8"),
            binding(HotkeyAction::RunOnce, "F8"),
        ];
        assert!(parse_bindings(&bindings).is_err());
    }

    #[test]
    fn duplicate_xinput_shortcuts_are_rejected_across_actions() {
        let bindings = vec![
            binding(HotkeyAction::ContinuousHold, "xinput:a"),
            binding(HotkeyAction::RunOnce, "xinput:a"),
        ];
        assert!(parse_bindings(&bindings).is_err());
    }

    #[test]
    fn keyboard_and_xinput_names_do_not_conflict() {
        let bindings = vec![
            binding(HotkeyAction::ContinuousHold, "A"),
            binding(HotkeyAction::RunOnce, "xinput:a"),
        ];
        assert!(parse_bindings(&bindings).is_ok());
    }

    #[test]
    fn xinput_shortcuts_are_normalized_and_displayed() {
        assert_eq!(canonical_shortcut("XINPUT:A").unwrap(), "xinput:a");
        assert_eq!(
            canonical_shortcut("xinput:dpad_up").unwrap(),
            "xinput:dpad_up"
        );
        assert_eq!(canonical_shortcut("xinput:lt").unwrap(), "xinput:lt");
        assert_eq!(display_shortcut("xinput:a"), "XInput A");
        assert_eq!(display_shortcut("xinput:dpad_up"), "XInput D-pad Up");
        assert_eq!(display_shortcut("xinput:lt"), "XInput LT");
    }

    #[test]
    fn unsupported_xinput_names_and_stick_axes_are_rejected() {
        assert!(canonical_shortcut("xinput:left_stick_x").is_err());
        assert!(canonical_shortcut("xinput:unknown").is_err());
    }

    #[test]
    fn canonical_keyboard_shortcut_uses_library_representation() {
        assert_eq!(canonical_shortcut("Ctrl+F8").unwrap(), "control+F8");
    }

    #[test]
    fn trigger_hysteresis_uses_requested_thresholds() {
        assert!(!trigger_is_pressed(127, false));
        assert!(trigger_is_pressed(128, false));
        assert!(trigger_is_pressed(102, true));
        assert!(!trigger_is_pressed(101, true));
    }

    #[test]
    fn button_edges_are_generated_once() {
        let mut tracker = XInputTracker::default();
        tracker.update(Some(0), Some(sample(0, 0, 0)));

        let pressed = tracker.update(Some(0), Some(sample(XINPUT_GAMEPAD_A, 0, 0)));
        assert_eq!(
            pressed,
            vec![TriggerEvent {
                id: TriggerId::XInput(XInputControl::A),
                state: TriggerState::Pressed,
            }]
        );
        assert!(
            tracker
                .update(Some(0), Some(sample(XINPUT_GAMEPAD_A, 0, 0)))
                .is_empty()
        );

        let released = tracker.update(Some(0), Some(sample(0, 0, 0)));
        assert_eq!(
            released,
            vec![TriggerEvent {
                id: TriggerId::XInput(XInputControl::A),
                state: TriggerState::Released,
            }]
        );
    }

    #[test]
    fn connecting_with_a_preheld_button_sets_baseline_without_press_edge() {
        let mut tracker = XInputTracker::default();
        let events = tracker.update(Some(0), Some(sample(XINPUT_GAMEPAD_A, 0, 0)));
        assert!(events.is_empty());
        assert!(tracker.pressed.contains(&XInputControl::A));
    }

    #[test]
    fn disconnect_releases_held_buttons() {
        let mut tracker = XInputTracker::default();
        tracker.update(Some(0), Some(sample(XINPUT_GAMEPAD_A, 0, 0)));
        let events = tracker.update(None, None);
        assert_eq!(
            events,
            vec![TriggerEvent {
                id: TriggerId::XInput(XInputControl::A),
                state: TriggerState::Released,
            }]
        );
    }

    #[test]
    fn slot_switch_releases_old_pad_and_baselines_new_pad() {
        let mut tracker = XInputTracker::default();
        tracker.update(Some(1), Some(sample(XINPUT_GAMEPAD_A, 0, 0)));
        let events = tracker.update(Some(2), Some(sample(XINPUT_GAMEPAD_B, 0, 0)));
        assert_eq!(
            events,
            vec![TriggerEvent {
                id: TriggerId::XInput(XInputControl::A),
                state: TriggerState::Released,
            }]
        );
        assert!(tracker.pressed.contains(&XInputControl::B));
        assert!(!tracker.pressed.contains(&XInputControl::A));
    }

    #[test]
    fn first_available_slot_is_selected_and_current_slot_is_sticky() {
        let states = [None, Some(sample(0, 0, 0)), Some(sample(0, 0, 0)), None];
        let first = select_xinput_with(None, |slot| states[slot as usize]);
        assert_eq!(first.map(|(slot, _)| slot), Some(1));

        let sticky = select_xinput_with(Some(2), |slot| states[slot as usize]);
        assert_eq!(sticky.map(|(slot, _)| slot), Some(2));

        let disconnected_current = [Some(sample(0, 0, 0)), Some(sample(0, 0, 0)), None, None];
        let fallback = select_xinput_with(Some(2), |slot| disconnected_current[slot as usize]);
        assert_eq!(fallback.map(|(slot, _)| slot), Some(0));
    }

    #[test]
    fn xinput_disconnect_releases_active_continuous_hold() {
        let mut tracker = XInputTracker::default();
        let mut active = HashSet::new();
        tracker.update(Some(0), Some(sample(0, 0, 0)));

        for event in tracker.update(Some(0), Some(sample(XINPUT_GAMEPAD_A, 0, 0))) {
            assert_eq!(update_hold_state(&mut active, event), Some(true));
        }
        assert!(!active.is_empty());

        let releases = tracker.update(None, None);
        assert_eq!(releases.len(), 1);
        assert_eq!(update_hold_state(&mut active, releases[0]), Some(false));
        assert!(active.is_empty());
    }

    #[test]
    fn disconnect_release_stops_the_last_active_hold() {
        let id = TriggerId::XInput(XInputControl::A);
        let mut active = HashSet::new();
        assert_eq!(
            update_hold_state(
                &mut active,
                TriggerEvent {
                    id,
                    state: TriggerState::Pressed,
                },
            ),
            Some(true)
        );
        assert_eq!(
            update_hold_state(
                &mut active,
                TriggerEvent {
                    id,
                    state: TriggerState::Released,
                },
            ),
            Some(false)
        );
        assert!(active.is_empty());
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

    #[test]
    fn capture_baseline_contains_preheld_inputs() {
        let held = baseline_pressed_controls(sample(XINPUT_GAMEPAD_A, 255, 0));
        assert!(held.contains(&XInputControl::A));
        assert!(held.contains(&XInputControl::LeftTrigger));
    }
}
