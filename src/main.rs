#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod config;
mod engine;
mod graphics;
mod hotkeys;
mod inference;
mod logging;
mod makcu;
mod models;
mod paths;
mod pipeline;
mod platform;
mod settings;
mod stats;
mod sync;
mod telemetry;
mod timing;
mod wait;

/// Asks the UI for a repaint. Any thread can call it. It does nothing while the window is minimized.
pub(crate) type Repaint = std::sync::Arc<dyn Fn() + Send + Sync>;

fn main() -> eframe::Result {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    let loaded = config::Config::load();
    let _logging = logging::init();
    logging::set_level(loaded.config.diagnostics.level);
    tracing::info!("nice-aimassist starting");
    platform::disable_process_power_throttling();
    if let Err(error) =
        graphics::bootstrap::initialize_graphics_diagnostics(loaded.config.diagnostics.gpu_debug)
    {
        tracing::error!(error = %crate::logging::chain(&error), "graphics diagnostics initialization failed");
        return Err(eframe::Error::AppCreation(error.into_boxed_dyn_error()));
    }
    // SAFETY: this sets process-wide DPI behavior before creating any windows.
    if unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }.is_err()
    {
        tracing::warn!("failed to set per-monitor-v2 DPI awareness");
    }

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([720.0, 820.0])
            .with_min_inner_size([640.0, 640.0]),
        ..Default::default()
    };

    let result = eframe::run_native(
        "nice-aimassist",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, loaded)))),
    );
    tracing::info!(?result, "nice-aimassist exiting");
    result
}
