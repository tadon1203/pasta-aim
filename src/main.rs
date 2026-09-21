#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod config;
#[cfg(windows)]
mod detector;
#[cfg(windows)]
mod engine;
#[cfg(windows)]
mod hotkeys;
#[cfg(windows)]
mod mak_output;
#[cfg(windows)]
mod metrics;
#[cfg(windows)]
mod models;
#[cfg(windows)]
mod paths;

#[cfg(windows)]
fn main() -> eframe::Result {
    win_desktop_duplication::set_process_dpi_awareness();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([720.0, 820.0])
            .with_min_inner_size([640.0, 640.0]),
        ..Default::default()
    };

    eframe::run_native(
        "nice-aimassist",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "nice-aimassist is Windows-only because it uses DXGI Desktop Duplication and DirectML."
    );
}
