use eframe::egui;

use super::{
    labels::{capture_phase_label, mak_phase_label, model_phase_label},
    widgets::{error_label, warn_label},
};
use crate::{
    app::App,
    config::LogLevel,
    engine::{Command, EngineStatus, LoopSnapshot, cpu_fallback_nodes, provider_display_name},
    graphics::DisplayInfo,
    inference::{EpAssignmentState, ep_assignment::EpAssignedNode},
    metrics::{MetricSummary, PerformanceSnapshot},
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

            ui.label("Frames dropped by gate");
            ui.label(format!("{:.1}%", tracking.discard_ratio * 100.0));
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
    pub(super) fn draw_diagnostics(&mut self, ui: &mut egui::Ui, status: &EngineStatus) {
        egui::CollapsingHeader::new("Diagnostics")
            .default_open(false)
            .show(ui, |ui| {
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
                        crate::logging::set_level(level);
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
                ui.add_space(6.0);
                draw_tracking(ui, &status.tracking);
                ui.add_space(6.0);
                egui::Grid::new("diagnostics_state")
                    .num_columns(2)
                    .spacing([16.0, 5.0])
                    .show(ui, |ui| {
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
                                .map(DisplayInfo::label)
                                .unwrap_or_else(|| "Unavailable".to_owned()),
                        );
                        ui.end_row();

                        ui.label("Inference adapter");
                        ui.label(
                            status
                                .capture
                                .selected
                                .as_ref()
                                .map(|display| display.adapter_name.clone())
                                .unwrap_or_else(|| "Unavailable".to_owned()),
                        );
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

                        ui.label("MAK state");
                        ui.label(mak_phase_label(status.mak.phase));
                        ui.end_row();

                        if let Some(port) = &status.mak.port {
                            ui.label("MAK port");
                            ui.label(port);
                            ui.end_row();
                        }
                        if let Some(firmware) = status.mak.firmware {
                            ui.label("MAK firmware");
                            ui.label(firmware.to_string());
                            ui.end_row();
                        }

                        ui.label("XInput hotkey pad");
                        ui.label(
                            self.hotkeys
                                .xinput_slot()
                                .map(|slot| format!("Slot {slot}"))
                                .unwrap_or_else(|| "Not connected".to_owned()),
                        );
                        ui.end_row();

                        let release_latency = self.hotkeys.keyboard_release_latency();
                        ui.label("Keyboard release lag");
                        if release_latency.samples == 0 {
                            ui.label("No measured hold releases");
                        } else {
                            ui.label(format!(
                                "{:.1}/{:.1} ms p50/p95 · {} samples{}",
                                release_latency.p50_ms,
                                release_latency.p95_ms,
                                release_latency.samples,
                                if release_latency.assist_active {
                                    " · release assist active"
                                } else {
                                    ""
                                }
                            ));
                        }
                        ui.end_row();

                        ui.label("FPS limit");
                        ui.label(self.config.fps_limit.value().to_string());
                        ui.end_row();

                        ui.label("Confidence threshold");
                        ui.label(format!("{}%", self.config.confidence_threshold.value()));
                        ui.end_row();

                        if let Some((_, _, confidence, class_id)) = status.run.last_target {
                            ui.label("Last detection");
                            ui.label(format!("conf={confidence:.3}, class={class_id}"));
                            ui.end_row();
                        }
                    });

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
                draw_performance(ui, &status.performance);

                ui.add_space(8.0);
                if ui.button("Copy diagnostics").clicked() {
                    ui.ctx().copy_text(self.diagnostics_text(status));
                }
            });
    }

    pub(super) fn diagnostics_text(&self, status: &EngineStatus) -> String {
        let display = status
            .capture
            .selected
            .as_ref()
            .map(DisplayInfo::label)
            .unwrap_or_else(|| "Unavailable".to_owned());
        let model = status
            .model
            .active_name
            .clone()
            .unwrap_or_else(|| "None".to_owned());
        let input = status
            .model
            .input_size
            .map(|(w, h)| match status.model.precision {
                Some(precision) => format!("{w}x{h} {}", precision.label()),
                None => format!("{w}x{h}"),
            })
            .unwrap_or_else(|| "Unknown".to_owned());
        let mak_routes = format!("mouse={}", status.mak.routes.mouse);
        let xinput = self
            .hotkeys
            .xinput_slot()
            .map(|slot| format!("slot {slot}"))
            .unwrap_or_else(|| "not connected".to_owned());
        let warning = self.config_load_warning.as_deref().unwrap_or("none");
        let save_error = self.config_save_error.as_deref().unwrap_or("none");
        let keyboard_release = self.hotkeys.keyboard_release_latency();
        format!(
            "nice-aimassist {}\nmodel: {model}\ninput: {input}\ninference adapter: {}\n{}\ncapture: {display}\nmak: {} · port={} · firmware={}\nmak routes: {mak_routes}\nxinput hotkey controller: {xinput}\nkeyboard release lag p50/p95: {:.1}/{:.1} ms ({} samples, assist={})\nfps limit: {}\nfresh/admitted FPS: {:.1}/{:.1}\nadmission drop: {:.1}%\nperformance samples: {}\npipeline p50/p95/p99: {:.2}/{:.2}/{:.2} ms\nacquire wait p50/p95: {:.2}/{:.2} ms\ncapture p50/p95: {:.2}/{:.2} ms\ncopy submit p50/p95: {:.2}/{:.2} ms\nGPU copy p50/p95: {:.2}/{:.2} ms\nGPU preprocess p50/p95: {:.2}/{:.2} ms\nqueue age p50/p95: {:.2}/{:.2} ms\ninference p50/p95: {:.2}/{:.2} ms\nGPU slot drops: {} · mailbox overwrites: {} · slots in flight: {}\ncausal discards: {}\nconfig warning: {warning}\nconfig save error: {save_error}",
            env!("CARGO_PKG_VERSION"),
            status
                .capture
                .selected
                .as_ref()
                .map(|display| display.adapter_name.as_str())
                .unwrap_or("Unavailable"),
            ep_assignment_copy_text(status),
            mak_phase_label(status.mak.phase),
            status.mak.port.as_deref().unwrap_or("unknown"),
            status
                .mak
                .firmware
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            keyboard_release.p50_ms,
            keyboard_release.p95_ms,
            keyboard_release.samples,
            keyboard_release.assist_active,
            self.config.fps_limit.value(),
            status.performance.capture_source.fresh_fps,
            status.performance.capture_source.admitted_fps,
            status.performance.capture_source.admission_drop_ratio * 100.0,
            status.performance.sample_count,
            status.performance.total.p50_ms,
            status.performance.total.p95_ms,
            status.performance.total.p99_ms,
            status.performance.acquire_wait.p50_ms,
            status.performance.acquire_wait.p95_ms,
            status.performance.capture.p50_ms,
            status.performance.capture.p95_ms,
            status.performance.copy_submit.p50_ms,
            status.performance.copy_submit.p95_ms,
            status.performance.gpu_copy.p50_ms,
            status.performance.gpu_copy.p95_ms,
            status.performance.preprocess.p50_ms,
            status.performance.preprocess.p95_ms,
            status.performance.queue_age.p50_ms,
            status.performance.queue_age.p95_ms,
            status.performance.inference.p50_ms,
            status.performance.inference.p95_ms,
            status.performance.capture_source.gpu_slot_busy_drops,
            status.performance.capture_source.mailbox_overwrites,
            status.performance.capture_source.slots_in_flight,
            status.performance.causal_discards,
        )
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

fn draw_ep_assignment_diagnostics(ui: &mut egui::Ui, status: &EngineStatus) {
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

fn ep_assignment_copy_text(status: &EngineStatus) -> String {
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
        "Fresh FPS {:.1} · Admitted FPS {:.1} · FPS admission drop {:.1}%",
        performance.capture_source.fresh_fps,
        performance.capture_source.admitted_fps,
        performance.capture_source.admission_drop_ratio * 100.0,
    ));
    ui.small(format!(
        "Accumulated {:.2} · GPU slot drops {} · Mailbox overwrites {} · Slots in flight {} · Causal discards {}",
        performance.capture_source.mean_accumulated_frames,
        performance.capture_source.gpu_slot_busy_drops,
        performance.capture_source.mailbox_overwrites,
        performance.capture_source.slots_in_flight,
        performance.causal_discards,
    ));
    if performance.sample_count == 0 {
        if let Some(cold) = performance.cold_run {
            ui.label(format!("Cold run: {:.2} ms total", cold.total_ms));
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
            metric_row(ui, "Pipeline", performance.total);
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
            metric_row(ui, "Queue age", performance.queue_age);
            metric_row(ui, "Inference", performance.inference);
            metric_row(ui, "Postprocess", performance.postprocess);
            metric_row(ui, "MAK write", performance.mak_write);
            metric_row(ui, "Cadence", performance.cadence);
        });

    if let Some(cold) = performance.cold_run {
        ui.small(format!("Cold run total: {:.2} ms", cold.total_ms));
    }
    ui.small("Pipeline: DXGI LastPresentTime → MAK write completion");
}

fn metric_row(ui: &mut egui::Ui, name: &str, metric: MetricSummary) {
    ui.label(name);
    ui.label(format!("{:.2} ms", metric.p50_ms));
    ui.label(format!("{:.2} ms", metric.p95_ms));
    ui.label(format!("{:.2} ms", metric.p99_ms));
    ui.end_row();
}
