pub mod clock;
pub mod engine;
pub mod hotkeys;
pub mod logging;
pub mod makcu;
pub mod pipeline;
pub mod platform;
pub mod settings;
mod stats;
mod sync;
pub mod telemetry;

/// Asks the UI for a repaint. Any thread can call it. It does nothing while the window is minimized.
pub type Repaint = std::sync::Arc<dyn Fn() + Send + Sync>;

/// What the other threads send to the UI.
#[derive(Debug)]
pub enum Message {
    Hotkeys(hotkeys::Event),
}

/// Sends messages to the UI. Any thread can hold a clone. `send` also asks for a repaint.
#[derive(Clone)]
pub struct UiSender {
    tx: std::sync::mpsc::Sender<Message>,
    repaint: Repaint,
}

impl std::fmt::Debug for UiSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiSender").finish_non_exhaustive()
    }
}

impl UiSender {
    pub fn new(tx: std::sync::mpsc::Sender<Message>, repaint: Repaint) -> Self {
        Self { tx, repaint }
    }

    /// A closed UI is not an error: the message is dropped.
    pub fn send(&self, message: Message) {
        if self.tx.send(message).is_ok() {
            (self.repaint)();
        }
    }
}
