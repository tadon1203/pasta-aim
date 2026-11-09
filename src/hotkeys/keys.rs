pub(super) const KEYS: &[(&str, u16)] = &[
    ("KeyA", 0x41),
    ("KeyB", 0x42),
    ("KeyC", 0x43),
    ("KeyD", 0x44),
    ("KeyE", 0x45),
    ("KeyF", 0x46),
    ("KeyG", 0x47),
    ("KeyH", 0x48),
    ("KeyI", 0x49),
    ("KeyJ", 0x4A),
    ("KeyK", 0x4B),
    ("KeyL", 0x4C),
    ("KeyM", 0x4D),
    ("KeyN", 0x4E),
    ("KeyO", 0x4F),
    ("KeyP", 0x50),
    ("KeyQ", 0x51),
    ("KeyR", 0x52),
    ("KeyS", 0x53),
    ("KeyT", 0x54),
    ("KeyU", 0x55),
    ("KeyV", 0x56),
    ("KeyW", 0x57),
    ("KeyX", 0x58),
    ("KeyY", 0x59),
    ("KeyZ", 0x5A),
    ("Digit0", 0x30),
    ("Digit1", 0x31),
    ("Digit2", 0x32),
    ("Digit3", 0x33),
    ("Digit4", 0x34),
    ("Digit5", 0x35),
    ("Digit6", 0x36),
    ("Digit7", 0x37),
    ("Digit8", 0x38),
    ("Digit9", 0x39),
    ("F1", 0x70),
    ("F2", 0x71),
    ("F3", 0x72),
    ("F4", 0x73),
    ("F5", 0x74),
    ("F6", 0x75),
    ("F7", 0x76),
    ("F8", 0x77),
    ("F9", 0x78),
    ("F10", 0x79),
    ("F11", 0x7A),
    ("F12", 0x7B),
    ("F13", 0x7C),
    ("F14", 0x7D),
    ("F15", 0x7E),
    ("F16", 0x7F),
    ("F17", 0x80),
    ("F18", 0x81),
    ("F19", 0x82),
    ("F20", 0x83),
    ("F21", 0x84),
    ("F22", 0x85),
    ("F23", 0x86),
    ("F24", 0x87),
    ("ArrowLeft", 0x25),
    ("ArrowUp", 0x26),
    ("ArrowRight", 0x27),
    ("ArrowDown", 0x28),
    ("Backspace", 0x08),
    ("Enter", 0x0D),
    ("Space", 0x20),
    ("Tab", 0x09),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("BracketLeft", 0xDB),
    ("BracketRight", 0xDD),
    ("Backquote", 0xC0),
    ("Equal", 0xBB),
    ("Comma", 0xBC),
    ("Backslash", 0xDC),
    ("Slash", 0xBF),
    ("Minus", 0xBD),
    ("Period", 0xBE),
    ("Semicolon", 0xBA),
    ("Quote", 0xDE),
    ("BrowserBack", 0xA6),
];

pub(super) const ESCAPE: u16 = 0x1B;
pub(super) const CONTROL: u16 = 0x11;
pub(super) const SHIFT: u16 = 0x10;
pub(super) const ALT: u16 = 0x12;

pub(super) fn key_name(code: u16) -> Option<&'static str> {
    KEYS.iter()
        .find(|(_, known)| *known == code)
        .map(|(name, _)| *name)
}

/// Bare letters and digits are accepted for older settings files.
pub(super) fn key_code(name: &str) -> Option<u16> {
    let alias = match name.as_bytes() {
        [letter] if letter.is_ascii_alphabetic() => format!("Key{}", char::from(*letter)),
        [digit] if digit.is_ascii_digit() => format!("Digit{}", char::from(*digit)),
        _ => name.to_owned(),
    };
    KEYS.iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(&alias))
        .map(|(_, code)| *code)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Pad {
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

pub(super) struct PadInfo {
    pub pad: Pad,
    pub slug: &'static str,
    pub label: &'static str,
    pub button: u16,
}

pub(super) static PADS: [PadInfo; 16] = [
    PadInfo {
        pad: Pad::A,
        slug: "a",
        label: "XInput A",
        button: 0x1000,
    },
    PadInfo {
        pad: Pad::B,
        slug: "b",
        label: "XInput B",
        button: 0x2000,
    },
    PadInfo {
        pad: Pad::X,
        slug: "x",
        label: "XInput X",
        button: 0x4000,
    },
    PadInfo {
        pad: Pad::Y,
        slug: "y",
        label: "XInput Y",
        button: 0x8000,
    },
    PadInfo {
        pad: Pad::LeftShoulder,
        slug: "lb",
        label: "XInput LB",
        button: 0x0100,
    },
    PadInfo {
        pad: Pad::RightShoulder,
        slug: "rb",
        label: "XInput RB",
        button: 0x0200,
    },
    PadInfo {
        pad: Pad::Back,
        slug: "back",
        label: "XInput Back",
        button: 0x0020,
    },
    PadInfo {
        pad: Pad::Start,
        slug: "start",
        label: "XInput Start",
        button: 0x0010,
    },
    PadInfo {
        pad: Pad::LeftThumb,
        slug: "left_stick",
        label: "XInput Left Stick",
        button: 0x0040,
    },
    PadInfo {
        pad: Pad::RightThumb,
        slug: "right_stick",
        label: "XInput Right Stick",
        button: 0x0080,
    },
    PadInfo {
        pad: Pad::DpadUp,
        slug: "dpad_up",
        label: "XInput D-pad Up",
        button: 0x0001,
    },
    PadInfo {
        pad: Pad::DpadDown,
        slug: "dpad_down",
        label: "XInput D-pad Down",
        button: 0x0002,
    },
    PadInfo {
        pad: Pad::DpadLeft,
        slug: "dpad_left",
        label: "XInput D-pad Left",
        button: 0x0004,
    },
    PadInfo {
        pad: Pad::DpadRight,
        slug: "dpad_right",
        label: "XInput D-pad Right",
        button: 0x0008,
    },
    PadInfo {
        pad: Pad::LeftTrigger,
        slug: "lt",
        label: "XInput LT",
        button: 0,
    },
    PadInfo {
        pad: Pad::RightTrigger,
        slug: "rt",
        label: "XInput RT",
        button: 0,
    },
];

impl Pad {
    pub(super) const ALL: [Self; 16] = [
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

    pub(super) fn info(self) -> &'static PadInfo {
        &PADS[self as usize]
    }

    pub(super) fn from_slug(slug: &str) -> Option<Self> {
        PADS.iter()
            .find(|info| info.slug == slug)
            .map(|info| info.pad)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct PadSample {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
}

const TRIGGER_PRESS_THRESHOLD: u8 = 128;
const TRIGGER_RELEASE_THRESHOLD: u8 = 102;

pub(super) fn trigger_is_pressed(value: u8, was_pressed: bool) -> bool {
    if was_pressed {
        value >= TRIGGER_RELEASE_THRESHOLD
    } else {
        value >= TRIGGER_PRESS_THRESHOLD
    }
}

pub(super) fn pressed(sample: PadSample, was: PadSet) -> PadSet {
    let mut down = PadSet::default();
    for pad in Pad::ALL {
        let is_down = match pad {
            Pad::LeftTrigger => trigger_is_pressed(sample.left_trigger, was.contains(pad)),
            Pad::RightTrigger => trigger_is_pressed(sample.right_trigger, was.contains(pad)),
            _ => sample.buttons & pad.info().button != 0,
        };
        if is_down {
            down.insert(pad);
        }
    }
    down
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct PadSet(u16);

impl PadSet {
    pub(super) fn contains(self, pad: Pad) -> bool {
        self.0 & (1 << pad as u16) != 0
    }

    pub(super) fn insert(&mut self, pad: Pad) {
        self.0 |= 1 << pad as u16;
    }

    pub(super) fn remove(&mut self, pad: Pad) {
        self.0 &= !(1 << pad as u16);
    }

    pub(super) fn iter(self) -> impl Iterator<Item = Pad> {
        Pad::ALL.into_iter().filter(move |pad| self.contains(*pad))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct KeySet([u64; 4]);

impl KeySet {
    pub(super) fn contains(self, code: u16) -> bool {
        self.0[usize::from(code / 64)] & (1 << (code % 64)) != 0
    }

    pub(super) fn insert(&mut self, code: u16) {
        self.0[usize::from(code / 64)] |= 1 << (code % 64);
    }

    pub(super) fn remove(&mut self, code: u16) {
        self.0[usize::from(code / 64)] &= !(1 << (code % 64));
    }
}
