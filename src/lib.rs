pub mod config;
pub mod engine;
pub mod graphics;
pub mod hotkeys;
pub mod inference;
pub mod logging;
pub mod makcu;
pub mod models;
mod paths;
mod pipeline;
pub mod platform;
pub mod settings;
mod stats;
mod sync;
pub mod telemetry;
mod timing;
mod wait;

/// Asks the UI for a repaint. Any thread can call it. It does nothing while the window is minimized.
pub type Repaint = std::sync::Arc<dyn Fn() + Send + Sync>;
