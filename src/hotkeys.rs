use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use eframe::egui;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};

use crate::{
    config::{HotkeyAction, HotkeyBinding},
    engine::EngineControl,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureResult {
    Cancelled,
    Captured(HotKey),
}

pub struct HotkeyRegistry {
    manager: Option<GlobalHotKeyManager>,
    registered: HashMap<u32, HotKey>,
    actions: Arc<RwLock<HashMap<u32, HotkeyAction>>>,
    suspended: Arc<AtomicBool>,
    reset_epoch: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
    control: EngineControl,
}

impl HotkeyRegistry {
    pub fn new(control: EngineControl, ui_ctx: egui::Context) -> Self {
        let manager = GlobalHotKeyManager::new().ok();
        let actions = Arc::new(RwLock::new(HashMap::new()));
        let suspended = Arc::new(AtomicBool::new(false));
        let reset_epoch = Arc::new(AtomicU64::new(0));
        let running = Arc::new(AtomicBool::new(true));

        let listener_actions = Arc::clone(&actions);
        let listener_suspended = Arc::clone(&suspended);
        let listener_reset_epoch = Arc::clone(&reset_epoch);
        let listener_running = Arc::clone(&running);
        let listener_control = control.clone();
        let listener = thread::Builder::new()
            .name("nice-aimassist-hotkeys".to_owned())
            .spawn(move || {
                let mut active_hold_ids = HashSet::new();
                let mut seen_reset_epoch = listener_reset_epoch.load(Ordering::Acquire);
                while listener_running.load(Ordering::Acquire) {
                    let reset_epoch = listener_reset_epoch.load(Ordering::Acquire);
                    if reset_epoch != seen_reset_epoch {
                        seen_reset_epoch = reset_epoch;
                        active_hold_ids.clear();
                        listener_control.set_continuous(false);
                        ui_ctx.request_repaint();
                    }

                    let event = match GlobalHotKeyEvent::receiver()
                        .recv_timeout(Duration::from_millis(250))
                    {
                        Ok(event) => event,
                        Err(_) => continue,
                    };

                    let reset_epoch = listener_reset_epoch.load(Ordering::Acquire);
                    if reset_epoch != seen_reset_epoch {
                        seen_reset_epoch = reset_epoch;
                        active_hold_ids.clear();
                        listener_control.set_continuous(false);
                        ui_ctx.request_repaint();
                    }
                    if listener_suspended.load(Ordering::Acquire) {
                        continue;
                    }

                    let action = listener_actions
                        .read()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .get(&event.id)
                        .copied();

                    match (action, event.state) {
                        (Some(HotkeyAction::ContinuousHold), HotKeyState::Pressed) => {
                            let was_empty = active_hold_ids.is_empty();
                            active_hold_ids.insert(event.id);
                            if was_empty {
                                listener_control.set_continuous(true);
                            }
                            ui_ctx.request_repaint();
                        }
                        (Some(HotkeyAction::ContinuousHold), HotKeyState::Released) => {
                            active_hold_ids.remove(&event.id);
                            if active_hold_ids.is_empty() {
                                listener_control.set_continuous(false);
                            }
                            ui_ctx.request_repaint();
                        }
                        (Some(HotkeyAction::RunOnce), HotKeyState::Pressed) => {
                            listener_control.run_once();
                            ui_ctx.request_repaint();
                        }
                        _ => {}
                    }
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
        }
    }

    pub fn replace_bindings(&mut self, bindings: &[HotkeyBinding]) -> Result<(), String> {
        let desired = parse_bindings(bindings)?;
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| "Global hotkey manager could not be created".to_owned())?;

        let mut newly_registered = Vec::new();
        for (id, (hotkey, _action)) in &desired {
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
            .filter(|(id, _)| !desired.contains_key(id))
            .map(|(_, hotkey)| *hotkey)
            .collect();
        for hotkey in removed {
            let _ = manager.unregister(hotkey);
        }

        self.registered = desired
            .iter()
            .map(|(id, (hotkey, _))| (*id, *hotkey))
            .collect();
        *self
            .actions
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = desired
            .into_iter()
            .map(|(id, (_, action))| (id, action))
            .collect();

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
}

impl Drop for HotkeyRegistry {
    fn drop(&mut self) {
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
    HotKey::from_str(shortcut)
        .map(|hotkey| hotkey.to_string())
        .map_err(|error| format!("Invalid shortcut: {error}"))
}

pub fn display_shortcut(shortcut: &str) -> String {
    shortcut
        .replace("control+", "Ctrl+")
        .replace("shift+", "Shift+")
        .replace("alt+", "Alt+")
        .replace("super+", "Win+")
        .replace("Key", "")
        .replace("Digit", "")
}

fn parse_bindings(
    bindings: &[HotkeyBinding],
) -> Result<HashMap<u32, (HotKey, HotkeyAction)>, String> {
    let mut parsed: HashMap<u32, (HotKey, HotkeyAction)> = HashMap::new();
    for binding in bindings {
        let hotkey = HotKey::from_str(&binding.shortcut)
            .map_err(|error| format!("Invalid {} shortcut: {error}", binding.action.label()))?;
        if let Some((_, existing_action)) = parsed.get(&hotkey.id()) {
            return Err(format!(
                "{} is already assigned to {}",
                display_hotkey(hotkey),
                existing_action.label()
            ));
        }
        parsed.insert(hotkey.id(), (hotkey, binding.action));
    }
    Ok(parsed)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_shortcuts_are_rejected_across_actions() {
        let bindings = vec![
            HotkeyBinding {
                action: HotkeyAction::ContinuousHold,
                shortcut: "F8".to_owned(),
            },
            HotkeyBinding {
                action: HotkeyAction::RunOnce,
                shortcut: "F8".to_owned(),
            },
        ];
        assert!(parse_bindings(&bindings).is_err());
    }

    #[test]
    fn canonical_shortcut_uses_library_representation() {
        assert_eq!(canonical_shortcut("Ctrl+F8").unwrap(), "control+F8");
    }
}
