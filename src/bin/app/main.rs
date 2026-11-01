#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ui;

use nice_aimassist::{config, graphics, logging, platform};

fn main() -> eframe::Result {
    let loaded = config::Config::load();
    let _logging = logging::init();
    logging::set_level(loaded.config.diagnostics.level);
    tracing::info!("nice-aimassist starting");
    platform::disable_process_power_throttling();
    if let Err(error) =
        graphics::bootstrap::initialize_graphics_diagnostics(loaded.config.diagnostics.gpu_debug)
    {
        tracing::error!(error = %nice_aimassist::logging::chain(&error), "graphics diagnostics initialization failed");
        return Err(eframe::Error::AppCreation(error.into_boxed_dyn_error()));
    }
    if !platform::enable_per_monitor_dpi_awareness() {
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
        Box::new(move |cc| Ok(Box::new(ui::App::new(cc, loaded)))),
    );
    tracing::info!(?result, "nice-aimassist exiting");
    result
}
