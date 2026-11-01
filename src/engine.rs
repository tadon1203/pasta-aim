//! The engine thread: it owns the pipeline, the aim, and the MAKCU link, and runs one frame at a time.

mod frame;
mod state;
mod stats;

pub use state::{CapturePhase, ModelPhase, OutputPhase, RunPhase, State};
pub use stats::{Percentiles, Rates, Stages, Stats};

use std::{
    path::Path,
    sync::{Arc, mpsc::TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use stats::{PUBLISH_INTERVAL, Recorder};

use crate::{
    aim::{Aim, AimSettings},
    channel::{Message, Receiver, Sender, channel},
    clock::{self, Clock, Wait},
    output::{self, Device, Output},
    pipeline::{DisplayInfo, Options, Pipeline, displays, init_thread},
    settings::{self, Settings},
};

/// How often the engine polls MAKCU connection state, when no other event wakes it.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(100);
/// How long the engine spins before a capture deadline. Only the capture deadline spins.
const CAPTURE_SPIN: Duration = Duration::from_millis(1);

/// Requests accepted by the engine thread.
#[derive(Debug)]
pub enum Command {
    /// The settings of the UI. The engine applies only what differs from its own.
    Settings(Settings),
    /// Loads a model from `models/` by its file name, and stops the run.
    LoadModel(String),
    /// Captures another display, and stops the run.
    SelectDisplay(settings::Display),
    /// `true` starts a Continuous run, `false` stops it.
    Hold(bool),
    RunOnce,
    /// Connects again: the output ports in order, or only the one that the user chose.
    ReconnectOutput,
    RescanDisplays,
    RetryCapture,
    Shutdown,
}

/// The engine thread. Dropping it stops the thread and waits for it.
#[derive(Debug)]
pub struct Engine {
    commands: Sender<Command>,
    join: Option<JoinHandle<()>>,
}

impl Engine {
    /// Starts the engine thread. It sends the state and the statistics to `ui`.
    ///
    /// # Panics
    ///
    /// Panics if the wait handle, the QPC clock, or the thread cannot be created.
    pub fn spawn(settings: &Settings, ui: Sender<Message>) -> Self {
        tracing::info!("spawning engine thread");
        let (wait, wake) = clock::wait().expect("failed to create the engine wait");
        let (commands, inbox) = channel(move || wake.wake());
        let clock = Clock::new().expect("failed to read the QPC frequency");
        let settings = settings.clone();
        let join = thread::Builder::new()
            .name("nice-aimassist-engine".to_owned())
            .spawn(move || {
                tracing::info!("engine thread started");
                let _boost = crate::platform::boost_current_thread("engine");
                Worker::start(ui, wait, clock, settings).run(&inbox);
            })
            .expect("failed to spawn the engine thread");
        Self {
            commands,
            join: Some(join),
        }
    }

    pub fn sender(&self) -> &Sender<Command> {
        &self.commands
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        tracing::info!("shutting down engine thread");
        self.commands.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// The active run. `started` is when it began: a frame whose capture was attempted before that is stale.
/// Its mode is `State::run.phase`.
#[derive(Debug, Clone, Copy)]
struct ActiveRun {
    started: Instant,
}

/// Whether the engine keeps going after a command or a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Shutdown,
}

/// The state of the engine thread. The engine keeps no copy of a fact that `State` holds.
struct Worker {
    ui: Sender<Message>,
    /// Wakes the engine for a command, the housekeeping, or the capture deadline.
    wait: Wait,
    /// The settings that the engine applies. Its display is the one the user chose.
    settings: Settings,
    /// The engine state. `publish` sends it to the UI after an edit.
    state: State,
    state_changed: bool,
    recorder: Recorder,
    /// When the statistics were last sent to the UI.
    stats_sent: Instant,
    /// The pipeline of the selected display and the model. `None` until a model is loaded, and after a fault.
    pipeline: Option<Pipeline>,
    /// The target and the move that each frame plans.
    aim: Aim,
    /// The MAKCU link, which the planned move is written to.
    output: Output,
    run: Option<ActiveRun>,
    /// The earliest start of the next capture in a Continuous run.
    next_capture: Instant,
    /// A GPU rebuild happened since the last frame that reached the output.
    rebuilt: bool,
    /// COM is initialized on this thread, so that DXGI can be used.
    com_ready: bool,
}

impl Worker {
    /// Runs on the engine thread: COM, the display catalog, the display, the MAKCU, and the model.
    fn start(ui: Sender<Message>, wait: Wait, clock: Clock, settings: Settings) -> Self {
        let com_error = init_thread().err().map(|error| {
            tracing::error!(error = %format_args!("{error:#}"), "engine thread COM initialization failed");
            format!("Engine thread COM initialization failed: {error:#}")
        });
        let mut output = Output::default();
        output.set_port(chosen_port(settings.makcu_port.as_deref()));
        let mut worker = Self {
            ui,
            wait,
            state: State::default(),
            state_changed: false,
            recorder: Recorder::new(clock),
            stats_sent: Instant::now(),
            pipeline: None,
            aim: Aim::new(aim_settings(&settings)),
            output,
            run: None,
            next_capture: Instant::now(),
            rebuilt: false,
            com_ready: com_error.is_none(),
            settings,
        };
        if let Some(message) = com_error {
            worker.edit(|state| {
                state.capture.phase = CapturePhase::Failed;
                state.capture.error = Some(message.clone());
                state.graphics.catalog_error = Some(message);
            });
        }
        if worker.com_ready {
            worker.refresh_catalog();
            worker.record_display(worker.settings.display);
        }
        worker.connect_output();
        if let Some(name) = worker.settings.model.clone() {
            worker.load_model(name);
        }
        worker
    }

    fn run(mut self, commands: &Receiver<Command>) {
        self.event_loop(commands);
        self.stop_run();
        self.publish();
        tracing::info!("engine thread stopped");
    }

    fn event_loop(&mut self, commands: &Receiver<Command>) {
        let mut next_housekeeping = Instant::now();
        loop {
            if self.drain(commands) == Flow::Shutdown {
                return;
            }
            let now = Instant::now();
            if now >= next_housekeeping {
                self.poll_output_link();
                next_housekeeping = now + HOUSEKEEPING_INTERVAL;
            }
            // The earlier of the housekeeping and the capture deadline wakes the loop.
            let mut deadline = next_housekeeping;
            let mut spin = Duration::ZERO;
            match self.pending_capture(now) {
                Some(capture) if capture <= deadline => {
                    deadline = capture;
                    spin = CAPTURE_SPIN;
                }
                None if self.run.is_some() => {
                    if self.step_frame(commands) == Flow::Shutdown {
                        return;
                    }
                    // The end of each pass sends the state, if it changed.
                    self.publish();
                    continue;
                }
                _ => {}
            }
            self.publish();
            self.wait.until(Some(deadline), spin);
        }
    }

    /// Applies the queued commands. A closed channel shuts the engine down.
    fn drain(&mut self, commands: &Receiver<Command>) -> Flow {
        loop {
            match commands.try_recv() {
                Ok(command) => {
                    if self.handle(command) == Flow::Shutdown {
                        return Flow::Shutdown;
                    }
                }
                Err(TryRecvError::Empty) => return Flow::Continue,
                Err(TryRecvError::Disconnected) => return Flow::Shutdown,
            }
        }
    }

    fn handle(&mut self, command: Command) -> Flow {
        tracing::debug!(?command, "engine command received");
        match command {
            Command::Settings(new) => self.apply_settings(new),
            Command::LoadModel(name) => self.load_model(name),
            Command::SelectDisplay(display) => {
                if display == self.settings.display
                    && self.state.capture.phase == CapturePhase::Ready
                {
                    // The same display is already captured: only the run stops, and the UI hears it.
                    self.stop_run();
                    self.ui.send(Message::DisplayChanged(display));
                } else {
                    self.select_display(display);
                }
            }
            Command::Hold(active) => self.hold(active),
            Command::RunOnce => self.run_once(),
            Command::ReconnectOutput => {
                tracing::info!("reconnecting MAKCU in engine");
                self.stop_run();
                self.connect_output();
            }
            Command::RescanDisplays => self.refresh_catalog(),
            Command::RetryCapture => self.retry_capture(),
            Command::Shutdown => {
                tracing::info!("engine shutdown requested");
                return Flow::Shutdown;
            }
        }
        Flow::Continue
    }

    /// Applies only the differences from the settings that the engine has.
    fn apply_settings(&mut self, mut new: Settings) {
        // The display changes only through `SelectDisplay`, so the engine keeps its own.
        new.display = self.settings.display;
        let old = std::mem::replace(&mut self.settings, new);

        if aim_settings(&old) != aim_settings(&self.settings) {
            self.aim.set_settings(aim_settings(&self.settings));
        }
        if old.confidence != self.settings.confidence
            && let Some(pipeline) = self.pipeline.as_mut()
        {
            pipeline.set_confidence(self.settings.confidence);
        }
        if old.makcu_port != self.settings.makcu_port {
            self.stop_run();
            self.output
                .set_port(chosen_port(self.settings.makcu_port.as_deref()));
            // The connect sends the stored interpolation.
            self.connect_output();
        } else if old.interpolation != self.settings.interpolation && self.output.is_connected() {
            // While disconnected the value only waits: the next connect sends it.
            self.send_interpolation();
        }
        if old.low_latency != self.settings.low_latency {
            self.stop_run();
            if let Some(name) = self.state.model.active_name.clone() {
                self.load_model(name);
            }
        }
    }

    fn hold(&mut self, active: bool) {
        if !active {
            self.stop_run();
        } else if self.state.run.phase != RunPhase::Continuous {
            self.start_run(RunPhase::Continuous);
        }
    }

    fn run_once(&mut self) {
        if self.state.run.phase == RunPhase::Continuous {
            tracing::warn!("single inference requested while continuous inference is active");
            self.set_run_error("Stop continuous inference before running once".to_owned());
            return;
        }
        self.start_run(RunPhase::OneShot);
    }

    fn start_run(&mut self, mode: RunPhase) {
        if let Some(reason) = self.state.blocked() {
            tracing::warn!(%reason, ?mode, "inference start blocked");
            self.set_run_error(reason.to_owned());
            return;
        }
        self.begin_run(mode);
    }

    /// Starts a run with no readiness check. `start_run` checks the readiness first.
    fn begin_run(&mut self, mode: RunPhase) {
        self.aim.reset();
        let now = Instant::now();
        // The first capture of a run is due at once.
        self.next_capture = now;
        self.recorder.run_started(now);
        if let Some(pipeline) = self.pipeline.as_mut() {
            pipeline.reset_timing();
        }
        self.run = Some(ActiveRun { started: now });
        if mode == RunPhase::Continuous {
            tracing::info!(
                fps_limit = self.settings.fps_limit.get(),
                "continuous inference started"
            );
        }
        self.edit(|state| {
            state.run.phase = mode;
            state.run.error = None;
        });
    }

    /// Stops any active run. Safe to call when idle.
    fn stop_run(&mut self) {
        if self.run.take().is_some() {
            tracing::info!("inference stopped");
        }
        self.aim.reset();
        if self.state.run.phase != RunPhase::Idle {
            self.edit(|state| state.run.phase = RunPhase::Idle);
        }
    }

    fn fail_run(&mut self, error: String) {
        tracing::error!(%error, "inference stopped after error");
        self.stop_run();
        self.set_run_error(error);
    }

    fn set_run_error(&mut self, error: String) {
        self.edit(|state| state.run.error = Some(error));
    }

    /// Loads `name` on the pipeline, or builds the pipeline with it. On failure the old model stays.
    fn load_model(&mut self, name: String) {
        self.stop_run();
        tracing::info!(model = %name, "loading model in engine");
        self.edit(|state| {
            state.model.phase = ModelPhase::Loading;
            state.model.loading_name = Some(name.clone());
            state.model.error = None;
            state.run.error = None;
        });
        // Loading blocks this thread; show the state before it starts.
        self.publish();

        match self.install_model(&name) {
            Ok(built) => {
                if let Some(pipeline) = built {
                    self.pipeline = Some(pipeline);
                }
                self.recorder.pipeline_changed();
                self.sync_pipeline_state();
                tracing::info!(model = %name, "model load succeeded");
                self.edit(|state| {
                    state.model.active_name = Some(name.clone());
                    state.model.loading_name = None;
                });
                self.ui.send(Message::ModelLoaded(name));
            }
            Err(error) => {
                let message = format!("{error:#}");
                tracing::error!(model = %name, %message, "model load failed");
                let phase = if self.pipeline.is_some() {
                    ModelPhase::Ready
                } else {
                    ModelPhase::Empty
                };
                self.edit(|state| {
                    state.model.phase = phase;
                    state.model.loading_name = None;
                    state.model.error = Some(message);
                });
            }
        }
    }

    /// The pipeline work of a model load. `Ok(None)` means the loaded pipeline took the model in place.
    fn install_model(&mut self, name: &str) -> anyhow::Result<Option<Pipeline>> {
        let path = settings::model_path(name)?;
        let options = self.options();
        match self.pipeline.as_mut() {
            Some(pipeline) => pipeline.set_model(&path, options).map(|()| None),
            None => self.build_pipeline(&self.settings.display, &path).map(Some),
        }
    }

    /// Captures `display` with the active model, and swaps it in only on success. Without a model it
    /// only records the display.
    fn select_display(&mut self, target: settings::Display) {
        tracing::info!(
            adapter = target.adapter,
            output = target.output,
            "changing capture selection"
        );
        self.stop_run();
        let Some(info) = self.catalog_display(&target) else {
            self.fail_capture_change("the display is not connected");
            return;
        };
        let built = match self.state.model.active_name.clone() {
            Some(name) => settings::model_path(&name)
                .and_then(|path| self.build_pipeline(&target, &path))
                .map(Some),
            None => Ok(None),
        };
        match built {
            Ok(Some(pipeline)) => {
                self.commit_pipeline(pipeline);
                self.edit(|state| state.capture.desktop_size = None);
            }
            Ok(None) => self.record_display_info(info),
            Err(error) => {
                self.fail_capture_change(&format!("{error:#}"));
                return;
            }
        }
        self.settings.display = target;
        self.ui.send(Message::DisplayChanged(target));
    }

    /// Records the display of the settings without a pipeline. A catalog miss is a capture failure.
    fn record_display(&mut self, display: settings::Display) {
        match self.catalog_display(&display) {
            Some(info) => self.record_display_info(info),
            None => self.edit(|state| {
                state.capture.phase = CapturePhase::Failed;
                state.capture.selected = None;
                state.capture.error = Some("The selected display is unavailable".to_owned());
            }),
        }
    }

    fn record_display_info(&mut self, info: DisplayInfo) {
        self.edit(|state| {
            state.capture.phase = CapturePhase::Ready;
            state.capture.selected = Some(info);
            state.capture.error = None;
        });
    }

    fn fail_capture_change(&mut self, reason: &str) {
        let message = format!("Display change failed: {reason}");
        tracing::error!(%message, "display change failed; active pipeline retained");
        let phase = if self.pipeline.is_some() {
            CapturePhase::Ready
        } else {
            CapturePhase::Failed
        };
        self.edit(|state| {
            state.capture.phase = phase;
            state.capture.error = Some(message);
        });
    }

    fn retry_capture(&mut self) {
        self.stop_run();
        self.refresh_catalog();
        self.select_display(self.settings.display);
    }

    fn refresh_catalog(&mut self) {
        if !self.com_ready {
            return;
        }
        match displays() {
            Ok(found) => self.edit(|state| {
                state.graphics.displays = found;
                state.graphics.catalog_error = None;
            }),
            Err(error) => {
                let error = format!("{error:#}");
                tracing::error!(%error, "graphics enumeration failed");
                self.edit(|state| {
                    state.graphics.displays.clear();
                    state.graphics.catalog_error = Some(error);
                });
            }
        }
    }

    fn catalog_display(&self, display: &settings::Display) -> Option<DisplayInfo> {
        self.state
            .graphics
            .displays
            .iter()
            .find(|info| {
                u64::from(info.adapter_luid) == display.adapter
                    && info.output_index == display.output
            })
            .cloned()
    }

    fn options(&self) -> Options {
        Options {
            confidence: self.settings.confidence,
            low_latency: self.settings.low_latency,
        }
    }

    fn build_pipeline(
        &self,
        display: &settings::Display,
        model: &Path,
    ) -> anyhow::Result<Pipeline> {
        anyhow::ensure!(self.com_ready, "graphics COM initialization unavailable");
        Pipeline::new(display, model, self.options())
    }

    /// Takes `pipeline` as the active one. The old pipeline is dropped after the new one is in place.
    fn commit_pipeline(&mut self, pipeline: Pipeline) {
        self.pipeline = Some(pipeline);
        self.recorder.pipeline_changed();
        self.sync_pipeline_state();
    }

    /// Copies what the pipeline holds into the state: the display, the model, and the precision.
    fn sync_pipeline_state(&mut self) {
        let Some(pipeline) = self.pipeline.as_ref() else {
            return;
        };
        let selected = pipeline.display().clone();
        let input_size = pipeline.input_size();
        let precision = pipeline.precision();
        let cpu_nodes = pipeline.cpu_nodes();
        self.edit(|state| {
            state.capture.phase = CapturePhase::Ready;
            state.capture.selected = Some(selected);
            state.capture.error = None;
            state.model.phase = ModelPhase::Ready;
            state.model.input_size = Some(input_size);
            state.model.precision = Some(precision);
            state.model.cpu_nodes = cpu_nodes;
            state.model.error = None;
        });
    }

    fn connect_output(&mut self) {
        let ports = output::ports();
        self.edit(|state| state.output.ports = ports);
        self.set_output_state(OutputPhase::Connecting, None, None);
        self.publish();
        match self.output.connect() {
            Ok(device) => {
                tracing::info!(port = %device.port, firmware = device.firmware, "MAKCU connected in engine");
                self.set_output_state(OutputPhase::Connected, Some(device), None);
                // The device can lose the value when it restarts, so each connect sends it.
                self.send_interpolation();
            }
            Err(error) => self.output_lost(format!("{error:#}")),
        }
    }

    /// Reports a lost link: it stops the run and shows a failure. The user connects again.
    fn poll_output_link(&mut self) {
        if self.output.link_lost() {
            self.output_lost("MAKCU disconnected".to_owned());
        }
    }

    /// The link ended, by a failed write, a missing reply, or the check. Every such end comes here.
    fn output_lost(&mut self, error: String) {
        tracing::error!(%error, "MAKCU operation failed");
        self.stop_run();
        self.set_output_state(OutputPhase::Failed, None, Some(error));
    }

    /// Sends the Move interpolation. A failure of the value is shown on the error line, and the run still
    /// starts. A failure of the link is a lost link.
    fn send_interpolation(&mut self) {
        let Err(error) = self.output.set_interpolation(self.settings.interpolation) else {
            return;
        };
        let reason = format!("{error:#}");
        if !self.output.is_connected() {
            self.output_lost(reason);
            return;
        }
        tracing::warn!(%reason, "move interpolation failed");
        self.set_run_error(reason);
    }

    /// The one place that edits the MAKCU state. Each phase sets the device and the error it has.
    fn set_output_state(
        &mut self,
        phase: OutputPhase,
        device: Option<Device>,
        error: Option<String>,
    ) {
        self.edit(|state| {
            state.output.phase = phase;
            state.output.device = device;
            state.output.error = error;
        });
    }

    /// Edits the state and marks it for the next `publish`.
    fn edit(&mut self, edit: impl FnOnce(&mut State)) {
        edit(&mut self.state);
        self.state_changed = true;
    }

    /// Sends the state to the UI after an edit. It also sends the statistics when they changed: while
    /// a run is active, not more often than `PUBLISH_INTERVAL`.
    fn publish(&mut self) {
        if std::mem::take(&mut self.state_changed) {
            self.ui.send(Message::State(Arc::new(self.state.clone())));
        }
        let due = self.run.is_none() || self.stats_sent.elapsed() >= PUBLISH_INTERVAL;
        if due && self.recorder.take_changed() {
            self.ui.send(Message::Stats(self.recorder.stats()));
            self.stats_sent = Instant::now();
        }
    }
}

/// The chosen MAKCU port. A blank port in the settings means auto-detect, as `None` does.
fn chosen_port(port: Option<&str>) -> Option<String> {
    port.filter(|port| !port.trim().is_empty())
        .map(str::to_owned)
}

fn aim_settings(settings: &Settings) -> AimSettings {
    AimSettings {
        sensitivity: settings.sensitivity,
        target_y_offset: settings.target_y_offset,
        max_delta: settings.max_delta,
    }
}

#[cfg(test)]
mod fixture {
    use super::*;
    use crate::{clock::Wake, output::fake::FakeMakcu};

    /// A worker with no GPU: the output is the fake MAKCU, and the state is a ready capture and model
    /// with a 100 by 100 input. The worker has no run and no pipeline.
    pub(super) fn worker() -> (Worker, FakeMakcu, Receiver<Message>, Wake) {
        let settings = Settings::default();
        let (output, fake) = Output::fake();
        let (ui, messages) = channel(|| {});
        let (wait, waker) = clock::wait().unwrap();
        let clock = Clock::new().unwrap();
        let mut state = State::default();
        state.capture.phase = CapturePhase::Ready;
        state.model.phase = ModelPhase::Ready;
        state.model.input_size = Some((100, 100));
        state.output.phase = OutputPhase::Connected;
        let worker = Worker {
            ui,
            wait,
            state,
            state_changed: false,
            recorder: Recorder::new(clock),
            stats_sent: Instant::now(),
            pipeline: None,
            aim: Aim::new(aim_settings(&settings)),
            output,
            run: None,
            next_capture: Instant::now(),
            rebuilt: false,
            com_ready: false,
            settings,
        };
        (worker, fake, messages, waker)
    }
}

#[cfg(test)]
mod tests {
    use super::{fixture::worker, state::BLOCK_MODEL, *};
    use crate::settings::{Display, Sensitivity};

    fn count_states(messages: &Receiver<Message>) -> usize {
        std::iter::from_fn(|| messages.try_recv().ok())
            .filter(|message| matches!(message, Message::State(_)))
            .count()
    }

    #[test]
    fn hold_without_a_model_sets_the_model_reason_and_starts_no_run() {
        let (mut worker, _fake, _messages, _waker) = worker();
        worker.state.model.phase = ModelPhase::Empty;
        worker.handle(Command::Hold(true));
        assert!(worker.run.is_none());
        assert_eq!(worker.state.run.error.as_deref(), Some(BLOCK_MODEL));
    }

    #[test]
    fn settings_with_a_new_sensitivity_change_the_aim_and_start_no_reload() {
        let (mut worker, _fake, messages, _waker) = worker();
        worker.state.model.active_name = Some("model.onnx".into());
        let detection = crate::pipeline::Detection::for_test(70.0, 50.0, 0);
        let before = worker.aim.update(Some(detection), (100, 100)).exact;
        worker.aim.reset();

        let mut settings = worker.settings.clone();
        settings.sensitivity = Sensitivity::new(Sensitivity::MAX);
        worker.handle(Command::Settings(settings));

        worker.aim.reset();
        let after = worker.aim.update(Some(detection), (100, 100)).exact;
        assert_ne!(before, after);
        assert!(worker.state.model.loading_name.is_none());
        assert!(
            std::iter::from_fn(|| messages.try_recv().ok())
                .all(|message| !matches!(message, Message::ModelLoaded(_)))
        );
    }

    #[test]
    fn settings_with_a_new_low_latency_value_start_a_model_reload() {
        let (mut worker, _fake, _messages, _waker) = worker();
        // The model file does not exist, so the reload runs and fails on the name.
        worker.state.model.active_name = Some("missing-model.onnx".into());
        let mut settings = worker.settings.clone();
        settings.low_latency = !settings.low_latency;
        worker.handle(Command::Settings(settings));
        assert!(worker.state.model.error.is_some());
    }

    #[test]
    fn select_display_during_a_run_stops_the_run() {
        let (mut worker, _fake, _messages, _waker) = worker();
        worker.begin_run(RunPhase::Continuous);
        worker.handle(Command::SelectDisplay(Display {
            adapter: 1,
            output: 9,
        }));
        assert!(worker.run.is_none());
        assert_eq!(worker.state.run.phase, RunPhase::Idle);
    }

    #[test]
    fn the_state_is_sent_once_after_a_change_and_not_after_a_pass_without_one() {
        let (mut worker, _fake, messages, _waker) = worker();
        // A run that is not ready sets the run error: that is a change.
        worker.handle(Command::RunOnce);
        worker.publish();
        assert_eq!(count_states(&messages), 1);

        worker.publish();
        assert_eq!(count_states(&messages), 0);
    }

    #[test]
    fn reconnect_output_with_the_fake_updates_the_device() {
        let (mut worker, _fake, _messages, _waker) = worker();
        worker.handle(Command::ReconnectOutput);
        assert!(worker.state.output.device.is_some());
    }
}
