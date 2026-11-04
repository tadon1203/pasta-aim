//! How threads talk. Every thread takes its input from one `Receiver<T>`. Any thread sends to it through a
//! `Sender<T>`, which also tells the receiving thread that something arrived: a wake-up for the engine and
//! the hotkeys thread, a repaint for the UI.

use std::sync::{Arc, mpsc};

use crate::{engine, hotkeys, settings::Display};

pub use mpsc::Receiver;

#[derive(Debug)]
// `Stats` is sent at most ten times each second, so its size does not matter here.
#[allow(clippy::large_enum_variant)]
pub enum Message {
    /// The engine state, when it changes.
    State(Arc<engine::State>),
    /// The statistics of the run, at most ten times each second.
    Stats(engine::Stats),
    /// A model loaded after the UI asked to be told. The name is the file in `models/`.
    ModelLoaded(String),
    /// The display capture changed. The UI saves the choice.
    DisplayChanged(Display),
    Hotkeys(hotkeys::Event),
}

/// The sending end of a channel. Any thread can hold a clone.
pub struct Sender<T> {
    tx: mpsc::Sender<T>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            notify: Arc::clone(&self.notify),
        }
    }
}

impl<T> std::fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sender").finish_non_exhaustive()
    }
}

impl<T> Sender<T> {
    /// Never blocks. A receiver that has gone is not an error: the value is dropped.
    /// A value that was sent is followed by a call of the notify function.
    pub fn send(&self, value: T) {
        if self.tx.send(value).is_ok() {
            (self.notify)();
        }
    }
}

pub fn channel<T>(notify: impl Fn() + Send + Sync + 'static) -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = mpsc::channel();
    (
        Sender {
            tx,
            notify: Arc::new(notify),
        },
        rx,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    #[test]
    fn send_notifies_once_for_each_value() {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&calls);
        let (tx, rx) = channel(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        tx.send(1);
        tx.clone().send(2);
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), [1, 2]);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn send_to_a_gone_receiver_is_ignored() {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&calls);
        let (tx, rx) = channel(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        drop(rx);
        tx.send(1);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
