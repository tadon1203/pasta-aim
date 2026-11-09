use eframe::egui;

use super::{App, controller_line, error_label, makcu_line, percent};
use nice_aimassist::{
    engine::{CapturePhase, ModelPhase, Percentiles, Rates, Stages, State, Stats},
    pipeline::DisplayInfo,
};

impl App {
    pub(super) fn draw_diagnostics(&mut self, ui: &mut egui::Ui, engine: &State, stats: &Stats) {
        egui::CollapsingHeader::new("Diagnostics")
            .default_open(false)
            .show(ui, |ui| {
                self.draw_state_grids(ui, engine, stats);

                if let Some(error) = &engine.capture.error {
                    error_label(ui, format!("Capture: {error}"));
                    if ui.button("Retry capture").clicked() {
                        self.retry_capture();
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
        let mouse_route = engine
            .output
            .device
            .as_ref()
            .is_some_and(|device| device.mouse_route);
        let makcu_routes = format!("mouse={mouse_route}");
        let warning = self.settings_warning.as_deref().unwrap_or("none");
        let save_error = self.settings_save_error.as_deref().unwrap_or("none");
        format!(
            "nice-aimassist {}
model: {model}
inference adapter: {}
capture: {display}
makcu: {}
makcu routes: {makcu_routes}
xinput hotkey controller: {}
fps limit: {}
fresh/processed FPS: {:.1}/{:.1}
performance samples: {}
pipeline p50/p95: {:.2}/{:.2} ms
inference p50/p95: {:.2}/{:.2} ms
settings warning: {warning}
settings save error: {save_error}",
            env!("CARGO_PKG_VERSION"),
            engine
                .capture
                .selected
                .as_ref()
                .map_or("Unavailable", |display| display.adapter_name.as_str()),
            makcu_line(engine.output.phase, engine.output.device.as_ref()),
            controller_line(self.controller_slot),
            self.settings.fps_limit.get(),
            stats.rates.fresh_fps,
            stats.rates.processed_fps,
            stats.stages.sample_count,
            stats.stages.total.p50,
            stats.stages.total.p95,
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
            metric_row(ui, "Inference", performance.inference);
        });
}

fn metric_row(ui: &mut egui::Ui, name: &str, metric: Percentiles) {
    ui.label(name);
    ui.label(format!("{:.2} ms", metric.p50));
    ui.label(format!("{:.2} ms", metric.p95));
    ui.end_row();
}
