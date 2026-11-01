use std::time::{Duration, Instant};

use windows_sys::Win32::UI::Input::XboxController::{
    XINPUT_GAMEPAD_A, XINPUT_GAMEPAD_B, XINPUT_GAMEPAD_BACK, XINPUT_GAMEPAD_DPAD_DOWN,
    XINPUT_GAMEPAD_DPAD_LEFT, XINPUT_GAMEPAD_DPAD_RIGHT, XINPUT_GAMEPAD_DPAD_UP,
    XINPUT_GAMEPAD_LEFT_SHOULDER, XINPUT_GAMEPAD_LEFT_THUMB, XINPUT_GAMEPAD_RIGHT_SHOULDER,
    XINPUT_GAMEPAD_RIGHT_THUMB, XINPUT_GAMEPAD_START, XINPUT_GAMEPAD_X, XINPUT_GAMEPAD_Y,
    XINPUT_STATE, XInputGetState,
};

const CONNECTED_POLL_INTERVAL: Duration = Duration::from_millis(5);
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(1);
const SLOT_COUNT: u32 = 4;
const TRIGGER_PRESS_THRESHOLD: u8 = 128;
const TRIGGER_RELEASE_THRESHOLD: u8 = 102;

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
    pub const ALL: [Self; 16] = [
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

    pub fn slug(self) -> &'static str {
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

    pub fn display_name(self) -> &'static str {
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

    pub fn shortcut(self) -> String {
        format!("xinput:{}", self.slug())
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
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
pub(super) struct Sample {
    packet_number: u32,
    buttons: u16,
    left_trigger: u8,
    right_trigger: u8,
}

/// The `XInput` controls that are down. One bit for each control, in `XInputControl::ALL` order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ControlSet(u16);

impl ControlSet {
    fn bit(control: XInputControl) -> u16 {
        1 << (control as u16)
    }

    pub fn contains(self, control: XInputControl) -> bool {
        self.0 & Self::bit(control) != 0
    }

    pub fn insert(&mut self, control: XInputControl) {
        self.0 |= Self::bit(control);
    }

    pub fn remove(&mut self, control: XInputControl) {
        self.0 &= !Self::bit(control);
    }

    pub fn iter(self) -> impl Iterator<Item = XInputControl> {
        XInputControl::ALL
            .into_iter()
            .filter(move |control| self.contains(*control))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EdgeState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Edge {
    pub control: XInputControl,
    pub state: EdgeState,
}

#[derive(Default)]
struct Tracker {
    slot: Option<u32>,
    packet_number: Option<u32>,
    pressed: ControlSet,
}

impl Tracker {
    fn update(&mut self, slot: Option<u32>, sample: Option<Sample>) -> Vec<Edge> {
        if slot != self.slot {
            let events = self.release_all();
            self.slot = slot;
            self.packet_number = sample.map(|sample| sample.packet_number);
            if let (Some(_), Some(sample)) = (slot, sample) {
                self.pressed = baseline_pressed_controls(sample);
            }
            return events;
        }

        let Some(sample) = sample else {
            self.slot = None;
            self.packet_number = None;
            return self.release_all();
        };

        if self.packet_number == Some(sample.packet_number) {
            return Vec::new();
        }
        self.packet_number = Some(sample.packet_number);

        let next = pressed_controls(sample, self.pressed);
        let events = edges(self.pressed, next);
        self.pressed = next;
        events
    }

    fn release_all(&mut self) -> Vec<Edge> {
        let events = edges(self.pressed, ControlSet::default());
        self.pressed = ControlSet::default();
        events
    }
}

/// The edges from `before` to `after`, in button order.
fn edges(before: ControlSet, after: ControlSet) -> Vec<Edge> {
    XInputControl::ALL
        .into_iter()
        .filter_map(
            |control| match (before.contains(control), after.contains(control)) {
                (false, true) => Some(Edge {
                    control,
                    state: EdgeState::Pressed,
                }),
                (true, false) => Some(Edge {
                    control,
                    state: EdgeState::Released,
                }),
                _ => None,
            },
        )
        .collect()
}

/// The controls that are down in `sample`. A trigger keeps its hysteresis from `was_pressed`.
fn pressed_controls(sample: Sample, was_pressed: ControlSet) -> ControlSet {
    let mut pressed = ControlSet::default();
    for control in XInputControl::ALL {
        if control_is_pressed(sample, control, was_pressed.contains(control)) {
            pressed.insert(control);
        }
    }
    pressed
}

pub(super) struct Poller {
    tracker: Tracker,
    next_poll: Instant,
    /// True while an `XInput` shortcut is bound or a recording is active.
    fast: bool,
}

impl Poller {
    pub fn new() -> Self {
        Self {
            tracker: Tracker::default(),
            next_poll: Instant::now(),
            fast: false,
        }
    }

    pub fn slot(&self) -> Option<u32> {
        self.tracker.slot
    }

    pub fn pressed(&self) -> ControlSet {
        self.tracker.pressed
    }

    pub fn next_poll(&self) -> Instant {
        self.next_poll
    }

    /// Sets whether a connected controller is polled at the fast rate. Turning it on polls at once.
    pub fn set_fast(&mut self, fast: bool, now: Instant) {
        if fast && !self.fast {
            self.next_poll = self.next_poll.min(now);
        }
        self.fast = fast;
    }

    fn interval(&self, connected: bool) -> Duration {
        if connected && self.fast {
            CONNECTED_POLL_INTERVAL
        } else {
            DISCOVERY_INTERVAL
        }
    }

    pub fn poll_if_due(&mut self, now: Instant) -> Option<Vec<Edge>> {
        if now < self.next_poll {
            return None;
        }
        Some(self.poll_now(now))
    }

    /// Reads the controller at once, whether or not a poll is due. Its edges come from the last snapshot.
    pub fn poll_now(&mut self, now: Instant) -> Vec<Edge> {
        // A slot that stops answering is released by `update`, and the first slot that answers takes over.
        let found = self
            .tracker
            .slot
            .and_then(|slot| read_slot(slot).map(|sample| (slot, sample)))
            .or_else(first_available_slot);
        let (slot, sample) = match found {
            Some((slot, sample)) => (Some(slot), Some(sample)),
            None => (None, None),
        };

        let events = self.tracker.update(slot, sample);
        self.next_poll = now + self.interval(slot.is_some());
        events
    }
}

pub(super) fn baseline_pressed_controls(sample: Sample) -> ControlSet {
    pressed_controls(sample, ControlSet::default())
}

fn first_available_slot() -> Option<(u32, Sample)> {
    select_first_with(read_slot)
}

fn select_first_with(mut read: impl FnMut(u32) -> Option<Sample>) -> Option<(u32, Sample)> {
    (0..SLOT_COUNT).find_map(|slot| read(slot).map(|sample| (slot, sample)))
}

fn read_slot(slot: u32) -> Option<Sample> {
    let mut state = XINPUT_STATE::default();
    // SAFETY: XInputGetState writes one XINPUT_STATE to this valid pointer. Callers constrain
    // `slot` to XInput's documented user indices 0..=3.
    let result = unsafe { XInputGetState(slot, &mut state) };
    (result == 0).then_some(Sample {
        packet_number: state.dwPacketNumber,
        buttons: state.Gamepad.wButtons,
        left_trigger: state.Gamepad.bLeftTrigger,
        right_trigger: state.Gamepad.bRightTrigger,
    })
}

fn control_is_pressed(sample: Sample, control: XInputControl, was_pressed: bool) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(packet: u32, buttons: u16, left_trigger: u8, right_trigger: u8) -> Sample {
        Sample {
            packet_number: packet,
            buttons,
            left_trigger,
            right_trigger,
        }
    }

    #[test]
    fn trigger_hysteresis_uses_requested_thresholds() {
        assert!(!trigger_is_pressed(127, false));
        assert!(trigger_is_pressed(128, false));
        assert!(trigger_is_pressed(102, true));
        assert!(!trigger_is_pressed(101, true));
    }

    #[test]
    fn unchanged_packet_skips_edge_work() {
        let mut tracker = Tracker::default();
        tracker.update(Some(0), Some(sample(1, 0, 0, 0)));
        assert_eq!(
            tracker
                .update(Some(0), Some(sample(1, XINPUT_GAMEPAD_A, 0, 0)))
                .len(),
            0
        );
    }

    #[test]
    fn button_edges_are_generated_once() {
        let mut tracker = Tracker::default();
        tracker.update(Some(0), Some(sample(1, 0, 0, 0)));
        let pressed = tracker.update(Some(0), Some(sample(2, XINPUT_GAMEPAD_A, 0, 0)));
        assert_eq!(
            pressed,
            vec![Edge {
                control: XInputControl::A,
                state: EdgeState::Pressed,
            }]
        );
        let released = tracker.update(Some(0), Some(sample(3, 0, 0, 0)));
        assert_eq!(
            released,
            vec![Edge {
                control: XInputControl::A,
                state: EdgeState::Released,
            }]
        );
    }

    #[test]
    fn disconnect_releases_held_buttons() {
        let mut tracker = Tracker::default();
        tracker.update(Some(0), Some(sample(1, XINPUT_GAMEPAD_A, 0, 0)));
        let events = tracker.update(None, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].control, XInputControl::A);
        assert_eq!(events[0].state, EdgeState::Released);
    }

    #[test]
    fn first_available_slot_is_selected() {
        let states = [
            None,
            Some(sample(1, 0, 0, 0)),
            Some(sample(1, 0, 0, 0)),
            None,
        ];
        let first = select_first_with(|slot| states[slot as usize]);
        assert_eq!(first.map(|(slot, _)| slot), Some(1));
    }

    #[test]
    fn poll_interval_is_fast_only_with_a_controller_and_a_wanted_input() {
        let mut poller = Poller::new();
        assert_eq!(poller.interval(true), Duration::from_secs(1));

        poller.set_fast(true, Instant::now());
        assert_eq!(poller.interval(true), Duration::from_millis(5));
        assert_eq!(poller.interval(false), Duration::from_secs(1));

        poller.set_fast(false, Instant::now());
        assert_eq!(poller.interval(true), Duration::from_secs(1));
    }

    #[test]
    fn preheld_inputs_are_baselined() {
        let held = baseline_pressed_controls(sample(1, XINPUT_GAMEPAD_A, 255, 0));
        assert!(held.contains(XInputControl::A));
        assert!(held.contains(XInputControl::LeftTrigger));
    }
}
