use eframe::egui;

use super::{
    labels::{capture_phase_label, makcu_phase_label, model_phase_label},
    widgets::{error_label, warn_label},
};
use crate::ui::App;
use nice_aimassist::{
    config::LogLevel,
    engine::{Command, EngineState, cpu_fallback_nodes, provider_display_name},
    graphics::DisplayInfo,
    inference::{EpAssignmentState, ep_assignment::EpAssignedNode},
    telemetry::{LiveTelemetry, LoopSnapshot, MetricSummary, PerformanceSnapshot},
};

fn draw_tracking(ui: &mut egui::Ui, tracking: &LoopSnapshot) {
    ui.strong("Tracking loop");
    if tracking.samples == 0 && tracking.locked_frames == 0 {
        ui.small("No data yet. Lock a target during a continuous run.");
        return;
    }
    egui::Grid::new("tracking_loop")
        .num_columns(2)
        .spacing([16.0, 5.0])
        .show(ui, |ui| {
            ui.label("Loop period p50/p95");
            ui.label(format!(
                "{:.1}/{:.1} ms ({} sends)",
                tracking.period_p50_ms, tracking.period_p95_ms, tracking.samples
            ));
            ui.end_row();

            ui.label("Error RMS (locked)");
            ui.label(format!(
                "{:.2} px ({} frames)",
                tracking.rms_error_px, tracking.locked_frames
            ));
            ui.end_row();

            ui.label("Overshoots");
            ui.label(tracking.overshoots.to_string());
            ui.end_row();

            ui.label("Time to converge last/p50");
            ui.label(format!(
                "{} / {}",
                format_ms(tracking.convergence_last_ms),
                format_ms(tracking.convergence_p50_ms)
            ));
            ui.end_row();
        });
}

fn format_ms(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), |ms| format!("{ms:.0} ms"))
}

impl App {
    pub(super) fn draw_diagnostics(
        &mut self,
        ui: &mut egui::Ui,
        status: &EngineState,
        live: &LiveTelemetry,
    ) {
        egui::CollapsingHeader::new("Diagnostics")
            .default_open(false)
            .show(ui, |ui| {
                self.draw_log_settings(ui);
                ui.add_space(6.0);
                draw_tracking(ui, &live.tracking);
                ui.add_space(6.0);
                self.draw_state_grids(ui, status, live);
                draw_ep_assignment_diagnostics(ui, status);

                let release_latency = self.hotkeys.keyboard_release_latency();
                if release_latency.samples >= 8 && release_latency.p95_ms > 25.0 {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!(
                            "Keyboard Hold release lag is high: {:.1} ms p95",
                            release_latency.p95_ms
                        ),
                    );
                }

                if let Some(error) = &status.capture.error {
                    error_label(ui, format!("Capture: {error}"));
                    if ui.button("Retry capture").clicked() {
                        self.engine.send(Command::RetryCapture);
                    }
                }

                ui.add_space(6.0);
                ui.strong("Performance");
                draw_performance(ui, &live.performance);

                ui.add_space(8.0);
                if ui.button("Copy diagnostics").clicked() {
                    ui.ctx().copy_text(self.diagnostics_text(status, live));
                }
            });
    }

    fn draw_log_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Log level");
            let mut level = self.config.diagnostics.level;
            egui::ComboBox::from_id_salt("log_level_selector")
                .selected_text(log_level_label(level))
                .show_ui(ui, |ui| {
                    for option in [
                        LogLevel::Off,
                        LogLevel::Error,
                        LogLevel::Warn,
                        LogLevel::Info,
                        LogLevel::Debug,
                        LogLevel::Trace,
                    ] {
                        ui.selectable_value(&mut level, option, log_level_label(option));
                    }
                });
            if level != self.config.diagnostics.level {
                self.config.diagnostics.level = level;
                nice_aimassist::logging::set_level(level);
                self.persist();
            }
        });
        ui.small("The selected level and more severe logs are written to the log file.");

        let mut gpu_debug = self.config.diagnostics.gpu_debug;
        if ui
            .checkbox(
                &mut gpu_debug,
                "D3D12 debug layer + DRED (restart required)",
            )
            .changed()
        {
            self.config.diagnostics.gpu_debug = gpu_debug;
            self.persist();
        }
        ui.small("Slows inference by ~1-2 ms. Turn off when measuring performance.");
        if self.config.diagnostics.gpu_debug != self.gpu_debug_at_startup {
            warn_label(ui, "Restart to apply.");
        }

        let mut frame_trace = self.config.diagnostics.frame_trace;
        if ui
            .checkbox(&mut frame_trace, "Write a per-frame trace for each run")
            .changed()
        {
            self.config.diagnostics.frame_trace = frame_trace;
            self.engine.send(Command::SetFrameTrace(frame_trace));
            self.persist();
        }
        ui.small("JSON Lines in logs/trace-*.jsonl, applied from the next run.");
    }

    /// The engine and input state, as two grids so that each stays a short block.
    fn draw_state_grids(&self, ui: &mut egui::Ui, status: &EngineState, live: &LiveTelemetry) {
        egui::Grid::new("diagnostics_engine_state")
            .num_columns(2)
            .spacing([16.0, 5.0])
            .show(ui, |ui| self.draw_engine_rows(ui, status));

        egui::Grid::new("diagnostics_input_state")
            .num_columns(2)
            .spacing([16.0, 5.0])
            .show(ui, |ui| draw_input_rows(ui, self, status, live));
    }

    fn draw_engine_rows(&self, ui: &mut egui::Ui, status: &EngineState) {
        ui.label("GPU debug");
        ui.label(if self.gpu_debug_at_startup {
            "On"
        } else {
            "Off"
        });
        ui.end_row();

        ui.label("Model state");
        ui.label(model_phase_label(status.model.phase));
        ui.end_row();

        if let Some(model) = &status.model.active_name {
            ui.label("Active model");
            ui.label(model);
            ui.end_row();
        }

        ui.label("Capture");
        ui.label(capture_phase_label(status.capture.phase));
        ui.end_row();

        ui.label("Capture source");
        ui.label(
            status
                .capture
                .selected
                .as_ref()
                .map_or_else(|| "Unavailable".to_owned(), DisplayInfo::label),
        );
        ui.end_row();

        ui.label("Inference adapter");
        ui.label(status.capture.selected.as_ref().map_or_else(
            || "Unavailable".to_owned(),
            |display| display.adapter_name.clone(),
        ));
        ui.end_row();

        if let Some(assignment) = &status.model.ep_assignment {
            ui.label("Actual assignment");
            ui.label(actual_backend_label(assignment));
            ui.end_row();
        }

        if let Some((width, height)) = status.capture.desktop_size {
            ui.label("Desktop frame");
            ui.label(format!("{width}×{height} BGRA8"));
            ui.end_row();
        }
    }

    pub(super) fn diagnostics_text(&self, status: &EngineState, live: &LiveTelemetry) -> String {
        let display = status
            .capture
            .selected
            .as_ref()
            .map_or_else(|| "Unavailable".to_owned(), DisplayInfo::label);
        let model = status
            .model
            .active_name
            .clone()
            .unwrap_or_else(|| "None".to_owned());
        let input = status.model.input_size.map_or_else(
            || "Unknown".to_owned(),
            |(w, h)| match status.model.precision {
                Some(precision) => format!("{w}x{h} {}", precision.label()),
                None => format!("{w}x{h}"),
            },
        );
        let makcu_routes = format!("mouse={}", status.makcu.routes.mouse);
        let xinput = self
            .hotkeys
            .xinput_slot()
            .map_or_else(|| "not connected".to_owned(), |slot| format!("slot {slot}"));
        let warning = self.config_load_warning.as_deref().unwrap_or("none");
        let save_error = self.config_save_error.as_deref().unwrap_or("none");
        let keyboard_release = self.hotkeys.keyboard_release_latency();
        format!(
            "nice-aimassist {}\nmodel: {model}\ninput: {input}\ninference adapter: {}\n{}\ncapture: {display}\nmakcu: {} · port={} · firmware={}\nmakcu routes: {makcu_routes}\nxinput hotkey controller: {xinput}\nkeyboard release lag p50/p95: {:.1}/{:.1} ms ({} samples, assist={})\nfps limit: {}\nfresh/processed FPS: {:.1}/{:.1}\nperformance samples: {}\npipeline p50/p95/p99: {:.2}/{:.2}/{:.2} ms\nacquire wait p50/p95: {:.2}/{:.2} ms\ncapture p50/p95: {:.2}/{:.2} ms\ncopy submit p50/p95: {:.2}/{:.2} ms\nGPU copy p50/p95: {:.2}/{:.2} ms\nGPU preprocess p50/p95: {:.2}/{:.2} ms\ninference p50/p95: {:.2}/{:.2} ms\nconfig warning: {warning}\nconfig save error: {save_error}",
            env!("CARGO_PKG_VERSION"),
            status
                .capture
                .selected
                .as_ref()
                .map_or("Unavailable", |display| display.adapter_name.as_str()),
            ep_assignment_copy_text(status),
            makcu_phase_label(status.makcu.phase),
            status.makcu.port.as_deref().unwrap_or("unknown"),
            status
                .makcu
                .firmware
                .map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
            keyboard_release.p50_ms,
            keyboard_release.p95_ms,
            keyboard_release.samples,
            keyboard_release.assist_active,
            self.config.fps_limit.value(),
            live.performance.capture_source.fresh_fps,
            live.performance.capture_source.processed_fps,
            live.performance.sample_count,
            live.performance.total.p50,
            live.performance.total.p95,
            live.performance.total.p99,
            live.performance.acquire_wait.p50,
            live.performance.acquire_wait.p95,
            live.performance.capture.p50,
            live.performance.capture.p95,
            live.performance.copy_submit.p50,
            live.performance.copy_submit.p95,
            live.performance.gpu_copy.p50,
            live.performance.gpu_copy.p95,
            live.performance.preprocess.p50,
            live.performance.preprocess.p95,
            live.performance.inference.p50,
            live.performance.inference.p95,
        )
    }
}

/// The input rows: MAKCU, `XInput`, keyboard release lag, and the value settings in use.
fn draw_input_rows(ui: &mut egui::Ui, app: &App, status: &EngineState, live: &LiveTelemetry) {
    ui.label("MAKCU state");
    ui.label(makcu_phase_label(status.makcu.phase));
    ui.end_row();

    if let Some(port) = &status.makcu.port {
        ui.label("MAKCU port");
        ui.label(port);
        ui.end_row();
    }
    if let Some(firmware) = status.makcu.firmware {
        ui.label("MAKCU firmware");
        ui.label(firmware.to_string());
        ui.end_row();
    }

    ui.label("XInput hotkey pad");
    ui.label(
        app.hotkeys
            .xinput_slot()
            .map_or_else(|| "Not connected".to_owned(), |slot| format!("Slot {slot}")),
    );
    ui.end_row();

    let release_latency = app.hotkeys.keyboard_release_latency();
    ui.label("Keyboard release lag");
    if release_latency.samples == 0 {
        ui.label("No measured hold releases");
    } else {
        let assist = if release_latency.assist_active {
            " · release assist active"
        } else {
            ""
        };
        ui.label(format!(
            "{:.1}/{:.1} ms p50/p95 · {} samples{assist}",
            release_latency.p50_ms, release_latency.p95_ms, release_latency.samples,
        ));
    }
    ui.end_row();

    ui.label("FPS limit");
    ui.label(app.config.fps_limit.value().to_string());
    ui.end_row();

    ui.label("Confidence threshold");
    ui.label(format!("{}%", app.config.confidence_threshold.value()));
    ui.end_row();

    if let Some((_, _, confidence, class_id)) = live.last_target {
        ui.label("Last detection");
        ui.label(format!("conf={confidence:.3}, class={class_id}"));
        ui.end_row();
    }
}

fn log_level_label(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Off => "Off",
        LogLevel::Error => "Error",
        LogLevel::Warn => "Warn",
        LogLevel::Info => "Info",
        LogLevel::Debug => "Debug",
        LogLevel::Trace => "Trace",
    }
}

fn actual_backend_label(state: &EpAssignmentState) -> String {
    match state {
        EpAssignmentState::Unavailable(_) => "Unavailable".to_owned(),
        EpAssignmentState::Available(report) => {
            let names = report
                .providers
                .iter()
                .filter(|provider| !provider.nodes.is_empty())
                .map(provider_display_name)
                .collect::<Vec<_>>();
            if names.is_empty() {
                "No assigned nodes".to_owned()
            } else {
                names.join(" + ")
            }
        }
    }
}

fn draw_ep_assignment_diagnostics(ui: &mut egui::Ui, status: &EngineState) {
    let Some(state) = &status.model.ep_assignment else {
        return;
    };
    ui.add_space(6.0);
    ui.strong("Execution");
    match &**state {
        EpAssignmentState::Unavailable(error) => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Assignment info unavailable: {error}"),
            );
        }
        EpAssignmentState::Available(report) => {
            ui.label(format!("Backend: {}", actual_backend_label(state)));
            ui.label(format!("Assigned nodes: {}", report.total_nodes));
            if let Some(summary) = &status.model.ep_assignment_summary {
                for (name, nodes, subgraphs) in [
                    (
                        "DirectML",
                        summary.directml_nodes,
                        summary.directml_subgraphs,
                    ),
                    ("CPU", summary.cpu_nodes, summary.cpu_subgraphs),
                ] {
                    let percent = if summary.total_nodes == 0 {
                        0.0
                    } else {
                        nodes as f64 / summary.total_nodes as f64 * 100.0
                    };
                    ui.label(format!(
                        "{name}: {nodes} nodes · {subgraphs} subgraphs · {percent:.1}%"
                    ));
                }
            }
            for provider in report.providers.iter().filter(|provider| {
                provider.name != "DmlExecutionProvider" && provider.name != "CPUExecutionProvider"
            }) {
                let percent = if report.total_nodes == 0 {
                    0.0
                } else {
                    provider.nodes.len() as f64 / report.total_nodes as f64 * 100.0
                };
                ui.label(format!(
                    "{}: {} nodes · {} subgraphs · {:.1}%",
                    provider_display_name(provider),
                    provider.nodes.len(),
                    provider.subgraph_count,
                    percent
                ));
            }
            ui.small("Node percentages describe assignment counts, not GPU work or performance.");

            let cpu_nodes = cpu_fallback_nodes(state);
            if cpu_nodes.is_empty() {
                ui.label("CPU fallback: None");
            } else {
                egui::CollapsingHeader::new(format!("CPU fallback nodes ({})", cpu_nodes.len()))
                    .default_open(false)
                    .show(ui, |ui| draw_assigned_nodes(ui, cpu_nodes));
            }

            egui::CollapsingHeader::new("All EP assignments")
                .default_open(false)
                .show(ui, |ui| {
                    for provider in &report.providers {
                        ui.strong(format!(
                            "{} ({})",
                            provider_display_name(provider),
                            provider.name
                        ));
                        draw_assigned_nodes(ui, &provider.nodes);
                    }
                });
        }
    }
}

fn draw_assigned_nodes(ui: &mut egui::Ui, nodes: &[EpAssignedNode]) {
    for node in nodes {
        ui.horizontal_wrapped(|ui| {
            ui.monospace(&node.op_type);
            ui.label(&node.name);
            if !node.domain.is_empty() {
                ui.small(format!("domain: {}", node.domain));
            }
        });
    }
}

fn ep_assignment_copy_text(status: &EngineState) -> String {
    let Some(state) = &status.model.ep_assignment else {
        return "Actual assignment: not available (no active model session)".to_owned();
    };
    match &**state {
        EpAssignmentState::Unavailable(error) => format!("Assignment info unavailable: {error}"),
        EpAssignmentState::Available(report) => {
            let mut lines = vec![
                format!("Inference backend: {}", actual_backend_label(state)),
                format!("Total assigned nodes: {}", report.total_nodes),
            ];
            for provider in &report.providers {
                lines.push(format!(
                    "{}: {} nodes / {} subgraphs",
                    provider.name,
                    provider.nodes.len(),
                    provider.subgraph_count
                ));
            }
            let cpu_nodes = cpu_fallback_nodes(state);
            if cpu_nodes.is_empty() {
                lines.push("CPU fallback: None".to_owned());
            } else {
                lines.push("CPU fallback:".to_owned());
                for node in cpu_nodes {
                    let domain = if node.domain.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", node.domain)
                    };
                    lines.push(format!("- {}: {}{}", node.op_type, node.name, domain));
                }
            }
            lines.join("\n")
        }
    }
}

fn draw_performance(ui: &mut egui::Ui, performance: &PerformanceSnapshot) {
    ui.label(format!(
        "Fresh FPS {:.1} · Processed FPS {:.1}",
        performance.capture_source.fresh_fps, performance.capture_source.processed_fps,
    ));
    ui.small(format!(
        "Capture timeouts {} / last reporting interval",
        performance.capture_source.timeouts,
    ));
    ui.small(format!(
        "Mean accumulated desktop frames {:.2}",
        performance.capture_source.mean_accumulated_frames,
    ));
    if performance.sample_count == 0 {
        if let Some(cold) = performance.cold_run {
            ui.label(format!("First frame: {:.2} ms total", cold.total));
        } else {
            ui.label("No performance samples yet");
        }
        return;
    }

    ui.label(format!(
        "{:.1} FPS · {} samples",
        performance.fps, performance.sample_count
    ));
    egui::Grid::new("performance_grid")
        .num_columns(4)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            ui.label("");
            ui.strong("p50");
            ui.strong("p95");
            ui.strong("p99");
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
            metric_row(ui, "MAKCU write", performance.makcu_write);
            metric_row(ui, "Cadence", performance.cadence);
        });

    if let Some(cold) = performance.cold_run {
        ui.small(format!("First frame total: {:.2} ms", cold.total));
    }
    ui.small("pipeline: DXGI LastPresentTime → MAKCU write completion");
}

fn metric_row(ui: &mut egui::Ui, name: &str, metric: MetricSummary) {
    ui.label(name);
    ui.label(format!("{:.2} ms", metric.p50));
    ui.label(format!("{:.2} ms", metric.p95));
    ui.label(format!("{:.2} ms", metric.p99));
    ui.end_row();
}
