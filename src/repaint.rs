//! The one place that the engine and the Telemetry thread use to ask the UI for a repaint.

use eframe::egui;

/// Asks for a repaint, unless the window is minimized. The windowing system repaints it when it comes back.
pub(crate) fn request_repaint(ctx: &egui::Context) {
    if ctx.input(|input| input.viewport().minimized) != Some(true) {
        ctx.request_repaint();
    }
}
