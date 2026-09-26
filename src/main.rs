#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod config;
#[cfg(windows)]
mod engine;
#[cfg(windows)]
mod graphics;
#[cfg(windows)]
mod hotkeys;
#[cfg(windows)]
mod inference;
#[cfg(windows)]
mod logging;
#[cfg(windows)]
mod mak;
#[cfg(windows)]
mod metrics;
#[cfg(windows)]
mod models;
#[cfg(windows)]
mod paths;
#[cfg(windows)]
mod settings;
#[cfg(windows)]
mod timing;

#[cfg(windows)]
fn main() -> eframe::Result {
    let loaded = config::Config::load();
    let _logging = logging::init();
    logging::set_level(loaded.config.diagnostics.level);
    tracing::info!("nice-aimassist starting");
    if let Err(error) =
        graphics::bootstrap::initialize_graphics_diagnostics(loaded.config.diagnostics.level)
    {
        tracing::error!(error = %format!("{error:#}"), "graphics diagnostics initialization failed");
        return Err(eframe::Error::AppCreation(error.into_boxed_dyn_error()));
    }
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
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

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "nice-aimassist is Windows-only because it uses DXGI Desktop Duplication and DirectML."
    );
}
