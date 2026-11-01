//! The Diagnostics panel, and the text that its Copy button puts on the clipboard.

use eframe::egui;

use super::{App, controller_line, error_label, makcu_line, percent, warn_label};
use nice_aimassist::{
    engine::{CapturePhase, Command, ModelPhase, Percentiles, Rates, Stages, State, Stats},
    pipeline::DisplayInfo,
    settings::LogLevel,
};

impl App {
    pub(super) fn draw_diagnostics(&mut self, ui: &mut egui::Ui, engine: &State, stats: &Stats) {
        egui::CollapsingHeader::new("Diagnostics")
            .default_open(false)
            .show(ui, |ui| {
                self.draw_log_settings(ui);
                ui.add_space(6.0);
                self.draw_state_grids(ui, engine, stats);

                if let Some(error) = &engine.capture.error {
                    error_label(ui, format!("Capture: {error}"));
                    if ui.button("Retry capture").clicked() {
                        self.commands.send(Command::RetryCapture);
                    }
                }

                ui.add_space(6.0);
                ui.strong("Performance");
                draw_performance(ui, &stats.stages, &stats.rates);

                ui.add_space(8.0);
                if ui.button("Copy diagnostics").clicked() {
                    ui.ctx().copy_text(self.diagnostics_text(engine, stats));
                }
            });
    }

    fn draw_log_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Log level");
            let mut level = self.settings.log_level;
            egui::ComboBox::from_id_salt("log_level_selector")
                .selected_text(level.label())
                .show_ui(ui, |ui| {
                    for option in LogLevel::ALL {
                        ui.selectable_value(&mut level, option, option.label());
                    }
                });
            if level != self.settings.log_level {
                self.settings.log_level = level;
                crate::logging::set_level(level);
                self.persist();
            }
        });
        ui.small("The selected level and more severe logs are written to the log file.");

        let mut gpu_debug = self.settings.gpu_debug;
        if ui
            .checkbox(&mut gpu_debug, "D3D12 debug layer (restart required)")
            .changed()
        {
            self.settings.gpu_debug = gpu_debug;
            self.persist();
        }
        ui.small("Slows inference by ~1-2 ms. Turn off when measuring performance.");
        if self.settings.gpu_debug != self.gpu_debug_at_startup {
            warn_label(ui, "Restart to apply.");
        }
    }

    /// The engine and input state, as two grids so that each stays a short block.
    fn draw_state_grids(&self, ui: &mut egui::Ui, engine: &State, stats: &Stats) {
        egui::Grid::new("diagnostics_engine_state")
            .num_columns(2)
            .spacing([16.0, 5.0])
            .show(ui, |ui| self.draw_engine_rows(ui, engine));

        egui::Grid::new("diagnostics_input_state")
            .num_columns(2)
            .spacing([16.0, 5.0])
            .show(ui, |ui| draw_input_rows(ui, self, engine, stats));
    }

    fn draw_engine_rows(&self, ui: &mut egui::Ui, engine: &State) {
        ui.label("GPU debug");
        ui.label(if self.gpu_debug_at_startup {
            "On"
        } else {
            "Off"
        });
        ui.end_row();

        ui.label("Model state");
        ui.label(model_phase_label(engine.model.phase));
        ui.end_row();

        if let Some(model) = &engine.model.active_name {
            ui.label("Active model");
            ui.label(model);
            ui.end_row();
        }

        ui.label("Capture");
        ui.label(capture_phase_label(engine.capture.phase));
        ui.end_row();

        ui.label("Capture source");
        ui.label(
            engine
                .capture
                .selected
                .as_ref()
                .map_or_else(|| "Unavailable".to_owned(), DisplayInfo::label),
        );
        ui.end_row();

        ui.label("Inference adapter");
        ui.label(engine.capture.selected.as_ref().map_or_else(
            || "Unavailable".to_owned(),
            |display| display.adapter_name.clone(),
        ));
        ui.end_row();

        ui.label("CPU fallback nodes");
        ui.label(cpu_fallback_value(engine));
        ui.end_row();

        if let Some((width, height)) = engine.capture.desktop_size {
            ui.label("Desktop frame");
            ui.label(format!("{width}×{height} BGRA8"));
            ui.end_row();
        }
    }

    pub(super) fn diagnostics_text(&self, engine: &State, stats: &Stats) -> String {
        let display = engine
            .capture
            .selected
            .as_ref()
            .map_or_else(|| "Unavailable".to_owned(), DisplayInfo::label);
        let model = engine
            .model
            .active_name
            .clone()
            .unwrap_or_else(|| "None".to_owned());
        let input = engine.model.input_size.map_or_else(
            || "Unknown".to_owned(),
            |(w, h)| match engine.model.precision {
                Some(precision) => format!("{w}x{h} {}", precision.label()),
                None => format!("{w}x{h}"),
            },
        );
        let mouse_route = engine
            .output
            .device
            .as_ref()
            .is_some_and(|device| device.mouse_route);
        let makcu_routes = format!("mouse={mouse_route}");
        let warning = self.settings_warning.as_deref().unwrap_or("none");
        let save_error = self.settings_save_error.as_deref().unwrap_or("none");
        format!(
            "nice-aimassist {}\nmodel: {model}\ninput: {input}\ninference adapter: {}\n{}\ncapture: {display}\nmakcu: {}\nmakcu routes: {makcu_routes}\nxinput hotkey controller: {}\nfps limit: {}\nfresh/processed FPS: {:.1}/{:.1}\nperformance samples: {}\npipeline p50/p95: {:.2}/{:.2} ms\nacquire wait p50/p95: {:.2}/{:.2} ms\ncapture p50/p95: {:.2}/{:.2} ms\ncopy submit p50/p95: {:.2}/{:.2} ms\nGPU copy p50/p95: {:.2}/{:.2} ms\nGPU preprocess p50/p95: {:.2}/{:.2} ms\ninference p50/p95: {:.2}/{:.2} ms\nsettings warning: {warning}\nsettings save error: {save_error}",
            env!("CARGO_PKG_VERSION"),
            engine
                .capture
                .selected
                .as_ref()
                .map_or("Unavailable", |display| display.adapter_name.as_str()),
            cpu_fallback_line(engine),
            makcu_line(engine.output.phase, engine.output.device.as_ref()),
            controller_line(self.controller_slot),
            self.settings.fps_limit.get(),
            stats.rates.fresh_fps,
            stats.rates.processed_fps,
            stats.stages.sample_count,
            stats.stages.total.p50,
            stats.stages.total.p95,
            stats.stages.acquire_wait.p50,
            stats.stages.acquire_wait.p95,
            stats.stages.capture.p50,
            stats.stages.capture.p95,
            stats.stages.copy_submit.p50,
            stats.stages.copy_submit.p95,
            stats.stages.gpu_copy.p50,
            stats.stages.gpu_copy.p95,
            stats.stages.preprocess.p50,
            stats.stages.preprocess.p95,
            stats.stages.inference.p50,
            stats.stages.inference.p95,
        )
    }
}

fn model_phase_label(phase: ModelPhase) -> &'static str {
    match phase {
        ModelPhase::Empty => "No active model",
        ModelPhase::Loading => "Loading",
        ModelPhase::Ready => "Ready",
    }
}

fn capture_phase_label(phase: CapturePhase) -> &'static str {
    match phase {
        CapturePhase::Initializing => "Initializing",
        CapturePhase::Ready => "Ready",
        CapturePhase::Failed => "Failed",
    }
}

/// The input rows: MAKCU, `XInput`, and the value settings in use.
fn draw_input_rows(ui: &mut egui::Ui, app: &App, engine: &State, stats: &Stats) {
    ui.label("MAKCU");
    ui.label(makcu_line(
        engine.output.phase,
        engine.output.device.as_ref(),
    ));
    ui.end_row();

    ui.label("XInput hotkey pad");
    ui.label(controller_line(app.controller_slot));
    ui.end_row();

    ui.label("FPS limit");
    ui.label(app.settings.fps_limit.get().to_string());
    ui.end_row();

    ui.label("Confidence threshold");
    ui.label(percent(app.settings.confidence.get()));
    ui.end_row();

    if let Some((_, _, confidence, class_id)) = stats.last_target {
        ui.label("Last detection");
        ui.label(format!("conf={confidence:.3}, class={class_id}"));
        ui.end_row();
    }
}

/// The CPU fallback value of the panel and the clipboard text: `N`, or `unknown` when ORT reports none.
fn cpu_fallback_value(engine: &State) -> String {
    engine
        .model
        .cpu_nodes
        .map_or_else(|| "unknown".to_owned(), |count| count.to_string())
}

/// The line that the clipboard text carries: `CPU fallback nodes: N`.
fn cpu_fallback_line(engine: &State) -> String {
    format!("CPU fallback nodes: {}", cpu_fallback_value(engine))
}

/// The rates, and when a run has samples, the timing grid of each stage.
fn draw_performance(ui: &mut egui::Ui, performance: &Stages, rates: &Rates) {
    ui.label(format!(
        "Fresh FPS {:.1} · Processed FPS {:.1}",
        rates.fresh_fps, rates.processed_fps,
    ));
    if performance.sample_count == 0 {
        ui.label("No performance samples yet");
        return;
    }

    ui.label(format!(
        "{:.1} FPS · {} samples",
        performance.fps, performance.sample_count
    ));
    draw_stage_grid(ui, performance);
    ui.small("pipeline: DXGI LastPresentTime → MAKCU write completion");
}

/// The p50 and p95 of each stage, from the capture to the MAKCU write.
fn draw_stage_grid(ui: &mut egui::Ui, performance: &Stages) {
    egui::Grid::new("performance_grid")
        .num_columns(3)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            ui.label("");
            ui.strong("p50");
            ui.strong("p95");
            ui.end_row();
            metric_row(ui, "pipeline", performance.total);
            metric_row(
                ui,
                "Present → GPU submit",
                performance.present_to_gpu_submit,
            );
            metric_row(
                ui,
                "Present → inference",
                performance.present_to_inference_start,
            );
            metric_row(ui, "DXGI acquire wait", performance.acquire_wait);
            metric_row(ui, "Capture", performance.capture);
            metric_row(ui, "GPU copy submit", performance.copy_submit);
            metric_row(ui, "GPU copy", performance.gpu_copy);
            metric_row(ui, "GPU preprocess", performance.preprocess);
            metric_row(ui, "Inference", performance.inference);
            metric_row(ui, "Postprocess", performance.postprocess);
            metric_row(ui, "MAKCU write", performance.output_write);
            metric_row(ui, "Cadence", performance.cadence);
        });
}

fn metric_row(ui: &mut egui::Ui, name: &str, metric: Percentiles) {
    ui.label(name);
    ui.label(format!("{:.2} ms", metric.p50));
    ui.label(format!("{:.2} ms", metric.p95));
    ui.end_row();
}
