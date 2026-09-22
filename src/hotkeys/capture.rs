use eframe::egui;
use global_hotkey::hotkey::HotKey;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureResult {
    Cancelled,
    Captured(HotKey),
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
