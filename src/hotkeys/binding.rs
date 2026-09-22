use std::{collections::HashMap, str::FromStr};

use global_hotkey::hotkey::HotKey;

use super::xinput::XInputControl;
use crate::settings::{HotkeyAction, HotkeyBinding};

#[derive(Default)]
pub(super) struct ParsedBindings {
    pub(super) keyboard: HashMap<u32, (HotKey, HotkeyAction)>,
    pub(super) xinput: HashMap<XInputControl, HotkeyAction>,
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

pub(super) fn parse_bindings(bindings: &[HotkeyBinding]) -> Result<ParsedBindings, String> {
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

pub(super) fn parse_xinput_shortcut(shortcut: &str) -> Result<Option<XInputControl>, String> {
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

pub(super) fn display_hotkey(hotkey: HotKey) -> String {
    display_shortcut(&hotkey.to_string())
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
        assert_eq!(display_shortcut("xinput:dpad_up"), "XInput D-pad Up");
        assert_eq!(display_shortcut("xinput:lt"), "XInput LT");
    }

    #[test]
    fn unsupported_stick_axes_are_rejected() {
        assert!(canonical_shortcut("xinput:left_stick_x").is_err());
    }
}
