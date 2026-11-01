use crate::sync::MutexExt;
mod binding;
mod capture;
mod listener;
mod xinput;

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager,
    hotkey::{Code, HotKey},
};

use crate::{
    Repaint,
    engine::{Command, EngineControl},
    settings::{HotkeyAction, HotkeyBinding},
    wait::{DeadlineWait, Waker},
};

pub use binding::{canonical_shortcut, display_shortcut};
use binding::{display_hotkey, parse_bindings};
pub use capture::{CaptureResult, capture_from_events};
use listener::Listener;
use xinput::XInputControl;

/// What the UI thread sends to the input thread. One ordered channel, then `Waker::wake`.
enum ListenerMessage {
    Key(GlobalHotKeyEvent),
    Bindings(BindingActions),
    /// Stops dispatch and releases every Hold shortcut.
    Suspend,
    Resume,
    /// Starts the recording of a Shortcut. The controls that are down now are ignored until they are released.
    BeginCapture,
    EndCapture,
    Shutdown,
}

#[derive(Default)]
struct BindingActions {
    keyboard: HashMap<u32, HotkeyAction>,
    keyboard_codes: HashMap<u32, Code>,
    xinput: HashMap<XInputControl, HotkeyAction>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct KeyboardReleaseLatency {
    pub samples: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub assist_active: bool,
}

/// What the input thread shows to the UI. The thread writes it only when a value changed.
#[derive(Debug, Clone, Default)]
struct InputView {
    xinput_slot: Option<u32>,
    release_latency: KeyboardReleaseLatency,
    /// `XInput` controls recorded during a capture and not yet taken by `poll_captured_xinput`.
    captured: VecDeque<XInputControl>,
}

pub struct HotkeyRegistry {
    manager: Option<GlobalHotKeyManager>,
    registered: HashMap<u32, HotKey>,
    tx: mpsc::Sender<ListenerMessage>,
    waker: Waker,
    input: Arc<Mutex<InputView>>,
    listener: Option<JoinHandle<()>>,
    control: EngineControl,
}

impl HotkeyRegistry {
    pub fn new(control: EngineControl, repaint: Repaint) -> Self {
        let manager = GlobalHotKeyManager::new().ok();
        if manager.is_some() {
            tracing::debug!("global hotkey manager initialized");
        } else {
            tracing::warn!(
                "global hotkey manager could not be initialized; keyboard bindings unavailable"
            );
        }

        let (wait, waker) = DeadlineWait::new().expect("failed to create the hotkey wait");
        let (tx, rx) = mpsc::channel();
        let input = Arc::new(Mutex::new(InputView::default()));

        // The hotkey manager calls this from its own thread. Each event is queued, then the input thread is woken.
        let handler_tx = tx.clone();
        let handler_waker = waker.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event| {
            let _ = handler_tx.send(ListenerMessage::Key(event));
            handler_waker.wake();
        }));

        let listener = Listener::new(rx, wait, control.clone(), repaint, Arc::clone(&input));
        let listener = thread::Builder::new()
            .name("nice-aimassist-inputs".to_owned())
            .spawn(move || listener.run())
            .ok();

        Self {
            manager,
            registered: HashMap::new(),
            tx,
            waker,
            input,
            listener,
            control,
        }
    }

    /// Sends the message, then wakes the input thread so that it reads it at once.
    fn send(&self, message: ListenerMessage) {
        let _ = self.tx.send(message);
        self.waker.wake();
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
        self.send(ListenerMessage::Bindings(BindingActions {
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
        }));
        tracing::debug!(
            keyboard = self.registered.len(),
            xinput = xinput_count,
            "hotkey bindings registered"
        );
        Ok(())
    }

    pub fn suspend(&self) {
        tracing::debug!("hotkey dispatch suspended");
        self.send(ListenerMessage::Suspend);
    }

    pub fn resume(&self) {
        tracing::debug!("hotkey dispatch resumed");
        self.send(ListenerMessage::Resume);
    }

    pub fn begin_capture(&self) {
        tracing::debug!("hotkey capture enabled");
        self.suspend();
        self.send(ListenerMessage::BeginCapture);
    }

    pub fn end_capture(&self) {
        tracing::debug!("hotkey capture disabled");
        self.send(ListenerMessage::EndCapture);
        self.resume();
    }

    pub fn poll_captured_xinput(&self) -> Option<String> {
        self.input
            .lock_recover()
            .captured
            .pop_front()
            .map(XInputControl::shortcut)
    }

    pub fn xinput_slot(&self) -> Option<u32> {
        self.input.lock_recover().xinput_slot
    }

    pub fn keyboard_release_latency(&self) -> KeyboardReleaseLatency {
        self.input.lock_recover().release_latency
    }
}

impl Drop for HotkeyRegistry {
    fn drop(&mut self) {
        tracing::debug!("shutting down hotkey registry");
        self.send(ListenerMessage::Shutdown);
        self.control.send(Command::SetContinuous(false));

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
