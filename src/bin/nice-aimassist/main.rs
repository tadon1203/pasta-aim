#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod logging;
mod process;
mod ui;

use anyhow::Result;
use nice_aimassist::{pipeline, settings::Settings};

fn main() -> Result<()> {
    // The logs folder is found first, so a missing install folder is reported before any settings file is read.
    let _logging = logging::init()?;
    let loaded = Settings::load();
    if let Some(warning) = &loaded.warning {
        tracing::warn!(%warning, "settings loaded with a problem");
    }
    logging::set_level(loaded.settings.log_level);
    tracing::info!("nice-aimassist starting");
    if let Err(error) = pipeline::init(loaded.settings.gpu_debug) {
        tracing::error!(error = %format_args!("{error:#}"), "GPU pipeline initialization failed");
        return Err(error);
    }
    process::disable_power_throttling();
    if !process::enable_per_monitor_dpi_awareness() {
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
    result.map_err(|error| anyhow::anyhow!("{error}"))
}
