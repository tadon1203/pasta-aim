//! Hotkeys: keyboard keys and `XInput` controls that run an action while they are held or once
//! when they are pressed. The hotkeys thread polls every shortcut each millisecond with
//! `GetAsyncKeyState` and `XInputGetState`. It never takes a key from another app, so a shortcut
//! key also reaches the focused window. The same thread records a new shortcut.

mod keys;

use std::{
    collections::{HashMap, HashSet},
    fmt,
    str::FromStr,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use windows::Win32::UI::Input::{
    KeyboardAndMouse::GetAsyncKeyState,
    XboxController::{XINPUT_STATE, XInputGetState},
};

use crate::{
    channel::{Message, Receiver, Sender, channel},
    clock, engine,
};
use keys::{ALT, CONTROL, ESCAPE, KeySet, Pad, PadSample, PadSet, SHIFT};

/// How often the hotkeys thread reads the inputs.
const POLL_INTERVAL: Duration = Duration::from_millis(1);
/// How often a disconnected controller is looked for.
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(1);
/// The number of `XInput` user slots.
const SLOT_COUNT: u32 = 4;

/// What a shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Runs while the shortcut is held.
    #[serde(rename = "continuous_hold")]
    Hold,
    /// Runs once when the shortcut is pressed.
    RunOnce,
}

impl Action {
    pub const ALL: [Self; 2] = [Self::Hold, Self::RunOnce];

    pub fn label(self) -> &'static str {
        match self {
            Self::Hold => "Hold continuous",
            Self::RunOnce => "Run once",
        }
    }
}

/// A physical input that a shortcut is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Input {
    Key(u16),
    Pad(Pad),
}

/// The modifier keys held with a keyboard key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
struct Mods {
    ctrl: bool,
    shift: bool,
    alt: bool,
}

/// A keyboard key with Ctrl, Shift, and Alt, or one `XInput` control (a button, LT, or RT).
/// `Display` and `FromStr` are the canonical text, such as `control+shift+KeyA` or `xinput:lt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shortcut {
    input: Input,
    mods: Mods,
}

impl Shortcut {
    fn new(input: Input, mods: Mods) -> Self {
        // A controller control has no modifiers.
        let mods = match input {
            Input::Key(_) => mods,
            Input::Pad(_) => Mods::default(),
        };
        Self { input, mods }
    }

    /// The text for a person: `Ctrl+Shift+A`, or `XInput LT`.
    pub fn label(self) -> String {
        match self.input {
            Input::Key(code) => {
                let mut label = String::new();
                if self.mods.ctrl {
                    label.push_str("Ctrl+");
                }
                if self.mods.shift {
                    label.push_str("Shift+");
                }
                if self.mods.alt {
                    label.push_str("Alt+");
                }
                let name = keys::key_name(code).unwrap_or_default();
                label.push_str(name.trim_start_matches("Key").trim_start_matches("Digit"));
                label
            }
            Input::Pad(pad) => pad.info().label.to_owned(),
        }
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.input {
            Input::Key(code) => {
                if self.mods.ctrl {
                    f.write_str("control+")?;
                }
                if self.mods.shift {
                    f.write_str("shift+")?;
                }
                if self.mods.alt {
                    f.write_str("alt+")?;
                }
                f.write_str(keys::key_name(code).unwrap_or_default())
            }
            Input::Pad(pad) => write!(f, "xinput:{}", pad.info().slug),
        }
    }
}

impl FromStr for Shortcut {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let normalized = text.trim().to_ascii_lowercase();
        if let Some(slug) = normalized.strip_prefix("xinput:") {
            let pad = Pad::from_slug(slug).ok_or_else(|| {
                anyhow!(
                    "Unsupported XInput shortcut: {text}. Sticks and unknown controller inputs are not supported."
                )
            })?;
            return Ok(Self::new(Input::Pad(pad), Mods::default()));
        }

        let mut parts: Vec<&str> = normalized.split('+').collect();
        let key = parts.pop().unwrap_or_default();
        let mut mods = Mods::default();
        for part in parts {
            match part {
                "control" | "ctrl" => mods.ctrl = true,
                "shift" => mods.shift = true,
                "alt" => mods.alt = true,
                _ => bail!("Invalid shortcut: unknown modifier {part} in {text}"),
            }
        }
        let code = keys::key_code(key).ok_or_else(|| anyhow!("Invalid shortcut: {text}"))?;
        Ok(Self::new(Input::Key(code), mods))
    }
}

impl Serialize for Shortcut {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Shortcut {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// One binding: an action and the shortcut that runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    pub action: Action,
    pub shortcut: Shortcut,
}

/// What the hotkeys thread reports to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A shortcut was recorded. Recording has ended.
    Recorded(Shortcut),
    /// Recording ended without a shortcut, because Esc was pressed or it was cancelled.
    RecordCancelled,
    /// The controller in use changed. `None` means no controller is connected.
    Controller(Option<u32>),
}

/// Requests accepted by the hotkeys thread.
#[derive(Debug)]
pub enum Command {
    /// Replaces the bindings. `bindings` checks them first.
    SetBindings(HashMap<Shortcut, Action>),
    /// Starts recording a shortcut. Dispatch stops until it ends.
    Record,
    /// Ends a recording without a shortcut.
    CancelRecord,
    Shutdown,
}

/// What one step of the hotkeys thread produces, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// A command for the engine: `Hold` while any Hold shortcut is down, `RunOnce` on each press.
    Action(Action, bool),
    /// A message for the UI.
    Event(Event),
}

/// The inputs read in one pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Reading {
    keys: KeySet,
    mods: Mods,
    escape: bool,
    slot: Option<u32>,
    pad: Option<PadSample>,
}

/// The controls that are ignored while a recording starts, until each one is released.
#[derive(Debug, Clone, Copy, Default)]
struct Ignored {
    keys: KeySet,
    pads: PadSet,
}

impl Ignored {
    fn contains(self, input: Input) -> bool {
        match input {
            Input::Key(code) => self.keys.contains(code),
            Input::Pad(pad) => self.pads.contains(pad),
        }
    }

    fn remove(&mut self, input: Input) {
        match input {
            Input::Key(code) => self.keys.remove(code),
            Input::Pad(pad) => self.pads.remove(pad),
        }
    }
}

/// The state of the hotkeys thread. It is pure: each pass is one `Reading`, and the output is the
/// actions and events that the pass produces.
#[derive(Debug, Default)]
pub(crate) struct Core {
    bindings: HashMap<Shortcut, Action>,
    /// The keyboard keys that the bindings use. A pass reads only these, unless a recording is on.
    bound_keys: KeySet,
    prev: Reading,
    /// The controls that are down, with the trigger hysteresis applied.
    pads: PadSet,
    /// The Hold shortcuts that are down. A Hold run is active while this is not empty.
    held: HashSet<Input>,
    recording: Option<Ignored>,
}

impl Core {
    pub(crate) fn new(bindings: HashMap<Shortcut, Action>) -> Self {
        Self {
            bound_keys: keys_of(&bindings),
            bindings,
            ..Self::default()
        }
    }

    /// The keyboard keys that the next pass must read, or `None` for every key. A recording takes any
    /// key. Otherwise only the keys of the bindings matter, and reading the others is wasted work.
    fn keys_to_read(&self) -> Option<KeySet> {
        self.recording.is_none().then_some(self.bound_keys)
    }

    /// Takes `keys` as the keys that were down before the next pass. The thread gives a reading of every
    /// key before a recording starts and before the bindings change, because a key that no pass read
    /// is not in the last reading: a key that is held at that moment must not look like a new press.
    fn set_down_keys(&mut self, keys: KeySet) {
        self.prev.keys = keys;
    }

    /// Applies one command. Returns `false` on `Shutdown`.
    fn command(&mut self, command: Command, out: &mut Vec<Signal>) -> bool {
        match command {
            Command::SetBindings(bindings) => {
                let before = self.holding();
                // A held control that no longer starts a Hold run stops being held.
                self.held.retain(|input| {
                    bindings.iter().any(|(shortcut, action)| {
                        shortcut.input == *input && *action == Action::Hold
                    })
                });
                self.bound_keys = keys_of(&bindings);
                self.bindings = bindings;
                self.settle(before, out);
            }
            Command::Record => {
                if self.recording.is_none() {
                    // Dispatch stops while recording: the Hold runs end, and the held controls are ignored.
                    let before = self.holding();
                    self.held.clear();
                    self.recording = Some(Ignored {
                        keys: self.prev.keys,
                        pads: self.pads,
                    });
                    self.settle(before, out);
                }
            }
            Command::CancelRecord => {
                if self.recording.take().is_some() {
                    out.push(Signal::Event(Event::RecordCancelled));
                }
            }
            Command::Shutdown => return false,
        }
        true
    }

    /// Turns one reading into the outputs of this pass.
    pub(crate) fn step(&mut self, reading: Reading, out: &mut Vec<Signal>) {
        let before = self.holding();
        let pads_now = reading
            .pad
            .map_or_else(PadSet::default, |sample| keys::pressed(sample, self.pads));

        if self.recording.is_some() && reading.escape && !self.prev.escape {
            self.recording = None;
            out.push(Signal::Event(Event::RecordCancelled));
        } else {
            self.edges(reading, pads_now, out);
        }

        self.prev = reading;
        self.pads = pads_now;
        self.settle(before, out);
    }

    fn edges(&mut self, reading: Reading, pads_now: PadSet, out: &mut Vec<Signal>) {
        let pads_before = self.pads;
        if reading.slot == self.prev.slot {
            for pad in pads_now.iter().filter(|pad| !pads_before.contains(*pad)) {
                self.press(Input::Pad(pad), Mods::default(), out);
            }
            for pad in pads_before.iter().filter(|pad| !pads_now.contains(*pad)) {
                self.release(Input::Pad(pad));
            }
        } else {
            out.push(Signal::Event(Event::Controller(reading.slot)));
            // The old controller's controls are released. The new one's are already down, so
            // they are a baseline: they make a Hold run active, but they are not presses.
            for pad in pads_before.iter() {
                self.release(Input::Pad(pad));
            }
            if self.recording.is_none() {
                for pad in pads_now.iter() {
                    let shortcut = Shortcut::new(Input::Pad(pad), Mods::default());
                    if self.bindings.get(&shortcut) == Some(&Action::Hold) {
                        self.held.insert(Input::Pad(pad));
                    }
                }
            }
        }

        for &(_, code) in keys::KEYS {
            let down = reading.keys.contains(code);
            let was_down = self.prev.keys.contains(code);
            if down && !was_down {
                self.press(Input::Key(code), reading.mods, out);
            } else if was_down && !down {
                self.release(Input::Key(code));
            }
        }
    }

    fn press(&mut self, input: Input, mods: Mods, out: &mut Vec<Signal>) {
        let shortcut = Shortcut::new(input, mods);
        if let Some(ignored) = self.recording {
            if !ignored.contains(input) {
                self.recording = None;
                out.push(Signal::Event(Event::Recorded(shortcut)));
            }
            return;
        }
        match self.bindings.get(&shortcut) {
            Some(Action::Hold) => {
                self.held.insert(input);
            }
            Some(Action::RunOnce) => out.push(Signal::Action(Action::RunOnce, true)),
            None => {}
        }
    }

    fn release(&mut self, input: Input) {
        if let Some(ignored) = self.recording.as_mut() {
            ignored.remove(input);
        }
        self.held.remove(&input);
    }

    fn holding(&self) -> bool {
        !self.held.is_empty()
    }

    /// Reports a change of the Hold state. Call it after every change to `held`.
    fn settle(&self, before: bool, out: &mut Vec<Signal>) {
        let now = self.holding();
        if now != before {
            out.push(Signal::Action(Action::Hold, now));
        }
    }
}

/// The keyboard keys that the shortcuts of `bindings` use. A modifier is read in every pass already.
fn keys_of(bindings: &HashMap<Shortcut, Action>) -> KeySet {
    let mut keys = KeySet::default();
    for shortcut in bindings.keys() {
        if let Input::Key(code) = shortcut.input {
            keys.insert(code);
        }
    }
    keys
}

/// Checks the bindings: a shortcut may belong to one action only.
pub fn bindings(hotkeys: &[Hotkey]) -> Result<HashMap<Shortcut, Action>> {
    let mut bindings = HashMap::new();
    for hotkey in hotkeys {
        if let Some(existing) = bindings.insert(hotkey.shortcut, hotkey.action) {
            bail!(
                "{} is already assigned to {}",
                hotkey.shortcut.label(),
                existing.label()
            );
        }
    }
    Ok(bindings)
}

/// The hotkeys thread. Dropping it stops the thread and waits for it.
#[derive(Debug)]
pub struct Hotkeys {
    commands: Sender<Command>,
    join: Option<JoinHandle<()>>,
}

impl Hotkeys {
    /// Starts the hotkeys thread. It sends `Hold` and `RunOnce` to `engine`, and what the user needs to
    /// see to `ui`. For `Hold`, the command is `Hold(true)` while any Hold shortcut is down.
    pub fn spawn(
        hotkeys: &[Hotkey],
        ui: Sender<Message>,
        engine: Sender<engine::Command>,
    ) -> Result<Self> {
        let bindings = bindings(hotkeys)?;
        let (wait, wake) = clock::wait()?;
        let (commands, inbox) = channel(move || wake.wake());
        let join = thread::Builder::new()
            .name("nice-aimassist-hotkeys".to_owned())
            .spawn(move || run(wait, &inbox, bindings, &ui, &engine))
            .context("failed to start the hotkeys thread")?;
        Ok(Self {
            commands,
            join: Some(join),
        })
    }

    pub fn sender(&self) -> &Sender<Command> {
        &self.commands
    }
}

impl Drop for Hotkeys {
    /// Stops and joins the hotkeys thread. The thread sends `Hold(false)` before it ends.
    fn drop(&mut self) {
        self.commands.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run(
    mut wait: clock::Wait,
    inbox: &Receiver<Command>,
    bindings: HashMap<Shortcut, Action>,
    ui: &Sender<Message>,
    engine: &Sender<engine::Command>,
) {
    tracing::info!("hotkeys thread started");
    let mut core = Core::new(bindings);
    let mut pads = PadPoll::new(Instant::now());
    let mut out = Vec::new();
    'passes: loop {
        wait.until(Some(Instant::now() + POLL_INTERVAL), Duration::ZERO);
        while let Ok(command) = inbox.try_recv() {
            if matches!(command, Command::Record | Command::SetBindings(_)) {
                // The keys that are held now are the baseline, also those that no pass reads.
                core.set_down_keys(read_keys(None));
            }
            let running = core.command(command, &mut out);
            flush(&mut out, ui, engine);
            if !running {
                break 'passes;
            }
        }
        let reading = read(&mut pads, Instant::now(), core.keys_to_read());
        core.step(reading, &mut out);
        flush(&mut out, ui, engine);
    }
    engine.send(engine::Command::Hold(false));
    tracing::info!("hotkeys thread stopped");
}

fn flush(out: &mut Vec<Signal>, ui: &Sender<Message>, engine: &Sender<engine::Command>) {
    for signal in out.drain(..) {
        match signal {
            Signal::Action(Action::Hold, down) => engine.send(engine::Command::Hold(down)),
            Signal::Action(Action::RunOnce, true) => engine.send(engine::Command::RunOnce),
            Signal::Action(Action::RunOnce, false) => {}
            Signal::Event(event) => ui.send(Message::Hotkeys(event)),
        }
    }
}

/// The keys of the table that are down. `only` limits the keys that are asked for, and `None` asks for all.
fn read_keys(only: Option<KeySet>) -> KeySet {
    let mut keys = KeySet::default();
    for &(_, code) in keys::KEYS {
        if only.is_none_or(|only| only.contains(code)) && key_down(code) {
            keys.insert(code);
        }
    }
    keys
}

/// Reads the keyboard and the controller. `only` is the keys that matter in this pass.
fn read(pads: &mut PadPoll, now: Instant, only: Option<KeySet>) -> Reading {
    let keys = read_keys(only);
    let (slot, pad) = pads.poll(now);
    Reading {
        keys,
        mods: Mods {
            ctrl: key_down(CONTROL),
            shift: key_down(SHIFT),
            alt: key_down(ALT),
        },
        escape: key_down(ESCAPE),
        slot,
        pad,
    }
}

fn key_down(code: u16) -> bool {
    // SAFETY: GetAsyncKeyState takes a virtual-key code by value and has no pointer arguments.
    unsafe { GetAsyncKeyState(i32::from(code)) }.cast_unsigned() & 0x8000 != 0
}

/// The controller in use. A connected one is read every pass; a disconnected one is looked for once a second.
struct PadPoll {
    slot: Option<u32>,
    next_probe: Instant,
}

impl PadPoll {
    fn new(now: Instant) -> Self {
        Self {
            slot: None,
            next_probe: now,
        }
    }

    fn poll(&mut self, now: Instant) -> (Option<u32>, Option<PadSample>) {
        if let Some(slot) = self.slot {
            if let Some(sample) = read_slot(slot) {
                return (Some(slot), Some(sample));
            }
            self.slot = None;
        }
        if now < self.next_probe {
            return (None, None);
        }
        self.next_probe = now + DISCOVERY_INTERVAL;
        match first_available_slot() {
            Some((slot, sample)) => {
                self.slot = Some(slot);
                (Some(slot), Some(sample))
            }
            None => (None, None),
        }
    }
}

fn first_available_slot() -> Option<(u32, PadSample)> {
    select_first_with(read_slot)
}

fn select_first_with(mut read: impl FnMut(u32) -> Option<PadSample>) -> Option<(u32, PadSample)> {
    (0..SLOT_COUNT).find_map(|slot| read(slot).map(|sample| (slot, sample)))
}

fn read_slot(slot: u32) -> Option<PadSample> {
    let mut state = XINPUT_STATE::default();
    // SAFETY: XInputGetState writes one XINPUT_STATE to this valid pointer. `slot` is 0..=3.
    let result = unsafe { XInputGetState(slot, &mut state) };
    (result == 0).then_some(PadSample {
        buttons: state.Gamepad.wButtons.0,
        left_trigger: state.Gamepad.bLeftTrigger,
        right_trigger: state.Gamepad.bRightTrigger,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const F8: u16 = 0x77;
    const F9: u16 = 0x78;
    const KEY_A: u16 = 0x41;

    const PAD_A_HELD: u16 = 0x1000;

    /// A reading with the keys down, and no controller.
    fn key_reading(keys: &[u16], mods: Mods) -> Reading {
        let mut set = KeySet::default();
        for code in keys {
            set.insert(*code);
        }
        Reading {
            keys: set,
            mods,
            ..Reading::default()
        }
    }

    /// A reading with the controller in slot 0, with these buttons and triggers.
    fn pad_reading(buttons: u16, left_trigger: u8, right_trigger: u8) -> Reading {
        Reading {
            slot: Some(0),
            pad: Some(PadSample {
                buttons,
                left_trigger,
                right_trigger,
            }),
            ..Reading::default()
        }
    }

    /// A reading with the keys down, and the controller in slot 0 with these buttons.
    fn reading(keys: &[u16], buttons: u16) -> Reading {
        Reading {
            keys: key_reading(keys, Mods::default()).keys,
            ..pad_reading(buttons, 0, 0)
        }
    }

    fn key(code: u16) -> Shortcut {
        Shortcut::new(Input::Key(code), Mods::default())
    }

    fn pad(pad: Pad) -> Shortcut {
        Shortcut::new(Input::Pad(pad), Mods::default())
    }

    fn core(bindings: &[(Shortcut, Action)]) -> Core {
        Core::new(bindings.iter().copied().collect())
    }

    fn step(core: &mut Core, reading: Reading) -> Vec<Signal> {
        let mut out = Vec::new();
        core.step(reading, &mut out);
        out
    }

    fn command(core: &mut Core, command: Command) -> Vec<Signal> {
        let mut out = Vec::new();
        core.command(command, &mut out);
        out
    }

    #[test]
    fn shortcut_text_is_canonical_and_round_trips() {
        assert_eq!("f8".parse::<Shortcut>().unwrap().to_string(), "F8");
        assert_eq!(
            "Control+Shift+KeyA"
                .parse::<Shortcut>()
                .unwrap()
                .to_string(),
            "control+shift+KeyA"
        );
        assert_eq!(
            "XINPUT:A".parse::<Shortcut>().unwrap().to_string(),
            "xinput:a"
        );
        assert_eq!(
            "xinput:dpad_up".parse::<Shortcut>().unwrap().label(),
            "XInput D-pad Up"
        );
        assert_eq!(
            "xinput:lt".parse::<Shortcut>().unwrap().label(),
            "XInput LT"
        );
    }

    #[test]
    fn invalid_shortcuts_are_rejected() {
        assert!("xinput:left_stick_x".parse::<Shortcut>().is_err());
        assert!(" ".parse::<Shortcut>().is_err());
        assert!("super+F8".parse::<Shortcut>().is_err());
        assert!("NotAKey".parse::<Shortcut>().is_err());
    }

    #[test]
    fn a_hotkey_serializes_with_the_existing_file_format() {
        let hotkey = Hotkey {
            action: Action::Hold,
            shortcut: "F8".parse().unwrap(),
        };
        let text = serde_json::to_string(&hotkey).unwrap();
        assert_eq!(text, r#"{"action":"continuous_hold","shortcut":"F8"}"#);
        let back: Hotkey = serde_json::from_str(&text).unwrap();
        assert_eq!(back, hotkey);
    }

    #[test]
    fn duplicate_keyboard_shortcuts_are_rejected_across_actions() {
        let hotkeys = [
            Hotkey {
                action: Action::Hold,
                shortcut: "F8".parse().unwrap(),
            },
            Hotkey {
                action: Action::RunOnce,
                shortcut: "F8".parse().unwrap(),
            },
        ];
        assert!(bindings(&hotkeys).is_err());
    }

    #[test]
    fn duplicate_xinput_shortcuts_are_rejected_across_actions() {
        let hotkeys = [
            Hotkey {
                action: Action::Hold,
                shortcut: "xinput:a".parse().unwrap(),
            },
            Hotkey {
                action: Action::RunOnce,
                shortcut: "xinput:a".parse().unwrap(),
            },
        ];
        assert!(bindings(&hotkeys).is_err());
    }

    #[test]
    fn keyboard_and_xinput_shortcuts_do_not_conflict() {
        let hotkeys = [
            Hotkey {
                action: Action::Hold,
                shortcut: "A".parse().unwrap(),
            },
            Hotkey {
                action: Action::RunOnce,
                shortcut: "xinput:a".parse().unwrap(),
            },
        ];
        assert!(bindings(&hotkeys).is_ok());
    }

    #[test]
    fn trigger_hysteresis_uses_requested_thresholds() {
        assert!(!keys::trigger_is_pressed(127, false));
        assert!(keys::trigger_is_pressed(128, false));
        assert!(keys::trigger_is_pressed(102, true));
        assert!(!keys::trigger_is_pressed(101, true));
    }

    #[test]
    fn button_edges_are_generated_once() {
        let mut core = core(&[(pad(Pad::A), Action::RunOnce)]);
        step(&mut core, pad_reading(0, 0, 0));
        let pressed = step(&mut core, pad_reading(PAD_A_HELD, 0, 0));
        assert_eq!(pressed, vec![Signal::Action(Action::RunOnce, true),]);
        assert_eq!(step(&mut core, pad_reading(PAD_A_HELD, 0, 0)), Vec::new());
        assert_eq!(step(&mut core, pad_reading(0, 0, 0)), Vec::new());
    }

    #[test]
    fn disconnect_releases_held_controls() {
        let mut core = core(&[(pad(Pad::A), Action::Hold)]);
        step(&mut core, pad_reading(0, 0, 0));
        let held = step(&mut core, pad_reading(PAD_A_HELD, 0, 0));
        assert!(held.contains(&Signal::Action(Action::Hold, true)));

        let lost = step(&mut core, Reading::default());
        assert!(lost.contains(&Signal::Event(Event::Controller(None))));
        assert!(lost.contains(&Signal::Action(Action::Hold, false)));
    }

    #[test]
    fn first_available_slot_is_selected() {
        let sample = PadSample::default();
        let states = [None, Some(sample), Some(sample), None];
        let first = select_first_with(|slot| states[slot as usize]);
        assert_eq!(first.map(|(slot, _)| slot), Some(1));
    }

    #[test]
    fn a_controller_held_at_connect_is_a_baseline_not_a_press() {
        let mut core = core(&[(pad(Pad::A), Action::RunOnce)]);
        let connected = step(&mut core, pad_reading(PAD_A_HELD, 0, 0));
        assert_eq!(connected, vec![Signal::Event(Event::Controller(Some(0)))]);
    }

    #[test]
    fn a_held_controller_at_connect_keeps_a_hold_run_active() {
        let mut core = core(&[(pad(Pad::A), Action::Hold)]);
        let connected = step(&mut core, pad_reading(PAD_A_HELD, 0, 0));
        assert!(connected.contains(&Signal::Action(Action::Hold, true)));
    }

    #[test]
    fn a_hold_run_stays_on_while_a_second_hold_shortcut_is_down() {
        let mut core = core(&[(key(F8), Action::Hold), (pad(Pad::A), Action::Hold)]);
        step(&mut core, reading(&[], 0));
        assert_eq!(
            step(&mut core, reading(&[F8], 0)),
            vec![Signal::Action(Action::Hold, true)]
        );
        // The second Hold shortcut goes down while the first is held: no change of state.
        assert_eq!(step(&mut core, reading(&[F8], PAD_A_HELD)), Vec::new());
        // The first is released while the second is still down: the run stays on.
        assert_eq!(step(&mut core, reading(&[], PAD_A_HELD)), Vec::new());
    }

    #[test]
    fn the_last_hold_release_stops_the_run() {
        let mut core = core(&[(key(F8), Action::Hold), (pad(Pad::A), Action::Hold)]);
        step(&mut core, reading(&[], 0));
        step(&mut core, reading(&[F8], 0));
        step(&mut core, reading(&[F8], PAD_A_HELD));
        step(&mut core, reading(&[F8], 0));
        assert_eq!(
            step(&mut core, reading(&[], 0)),
            vec![Signal::Action(Action::Hold, false)]
        );
    }

    #[test]
    fn recording_ignores_a_control_that_was_down_at_the_start_until_released() {
        let mut core = core(&[]);
        step(&mut core, pad_reading(PAD_A_HELD, 0, 0));
        command(&mut core, Command::Record);

        assert_eq!(step(&mut core, pad_reading(PAD_A_HELD, 0, 0)), Vec::new());
        assert_eq!(step(&mut core, pad_reading(0, 0, 0)), Vec::new());
        assert_eq!(
            step(&mut core, pad_reading(PAD_A_HELD, 0, 0)),
            vec![Signal::Event(Event::Recorded(pad(Pad::A)))]
        );
    }

    #[test]
    fn recording_takes_a_key_with_modifiers() {
        let mut core = core(&[]);
        command(&mut core, Command::Record);
        let mods = Mods {
            ctrl: true,
            shift: true,
            alt: false,
        };
        let recorded = step(&mut core, key_reading(&[KEY_A], mods));
        assert_eq!(
            recorded,
            vec![Signal::Event(Event::Recorded(Shortcut::new(
                Input::Key(KEY_A),
                mods
            )))]
        );
    }

    #[test]
    fn esc_during_recording_cancels_and_starts_no_action() {
        let mut core = core(&[(key(F8), Action::RunOnce)]);
        command(&mut core, Command::Record);
        let escape = Reading {
            escape: true,
            ..Reading::default()
        };
        assert_eq!(
            step(&mut core, escape),
            vec![Signal::Event(Event::RecordCancelled)]
        );
        assert!(core.recording.is_none());
    }

    #[test]
    fn no_action_fires_while_recording() {
        let mut core = core(&[(key(F9), Action::RunOnce), (key(F8), Action::Hold)]);
        command(&mut core, Command::Record);
        let out = step(&mut core, key_reading(&[F9], Mods::default()));
        assert!(
            out.iter()
                .all(|output| !matches!(output, Signal::Action(..)))
        );
        assert_eq!(out, vec![Signal::Event(Event::Recorded(key(F9)))]);
    }

    #[test]
    fn starting_a_recording_ends_a_running_hold() {
        let mut core = core(&[(key(F8), Action::Hold)]);
        step(&mut core, key_reading(&[F8], Mods::default()));
        assert_eq!(
            command(&mut core, Command::Record),
            vec![Signal::Action(Action::Hold, false)]
        );
    }

    #[test]
    fn a_pass_reads_only_the_bound_keys_and_a_recording_reads_every_key() {
        let mut core = core(&[(key(F8), Action::Hold), (pad(Pad::A), Action::RunOnce)]);
        let bound = core.keys_to_read().expect("the bound keys");
        assert!(bound.contains(F8));
        assert!(!bound.contains(F9) && !bound.contains(KEY_A));

        command(&mut core, Command::Record);
        assert_eq!(core.keys_to_read(), None);
        step(&mut core, key_reading(&[KEY_A], Mods::default()));
        assert!(core.keys_to_read().is_some(), "the recording ended");
    }

    #[test]
    fn new_bindings_change_the_keys_that_are_read() {
        let mut core = core(&[(key(F8), Action::Hold)]);
        let bindings = [(key(F9), Action::RunOnce)].into_iter().collect();
        command(&mut core, Command::SetBindings(bindings));
        let bound = core.keys_to_read().expect("the bound keys");
        assert!(bound.contains(F9) && !bound.contains(F8));
    }

    #[test]
    fn a_key_that_is_held_when_the_bindings_change_is_not_a_press() {
        let mut core = core(&[]);
        let mut held = KeySet::default();
        held.insert(F9);
        // The thread reads every key before it applies the new bindings.
        core.set_down_keys(held);
        let bindings = [(key(F9), Action::RunOnce)].into_iter().collect();
        command(&mut core, Command::SetBindings(bindings));
        assert_eq!(
            step(&mut core, key_reading(&[F9], Mods::default())),
            Vec::new()
        );
    }

    #[test]
    fn a_shutdown_request_stops_the_thread_loop() {
        let mut core = core(&[]);
        let mut out = Vec::new();
        assert!(!core.command(Command::Shutdown, &mut out));
    }
}
