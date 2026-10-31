//! The one way that the engine, the Telemetry thread, and the Input thread ask the UI for a repaint.

use eframe::egui;

/// Asks the UI for a repaint. Any thread can hold a clone.
#[derive(Clone)]
pub(crate) struct RepaintSignal {
    ctx: egui::Context,
}

impl RepaintSignal {
    pub(crate) fn new(ctx: egui::Context) -> Self {
        Self { ctx }
    }

    /// Asks for a repaint, unless the window is minimized. The windowing system repaints it when it comes back.
    pub(crate) fn request(&self) {
        if self.ctx.input(|input| input.viewport().minimized) != Some(true) {
            self.ctx.request_repaint();
        }
    }
}

#[cfg(test)]
impl RepaintSignal {
    /// A signal for tests. No UI receives it.
    pub(crate) fn detached() -> Self {
        Self::new(egui::Context::default())
    }
}
