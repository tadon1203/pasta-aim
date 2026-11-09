mod boost;
mod graphics;
mod link;
mod state;
mod stats;
mod step;

pub use state::{CapturePhase, ModelPhase, OutputPhase, RunPhase, State};
pub use stats::{Percentiles, Rates, Stages, Stats};

use std::{
    sync::{Arc, mpsc::TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use boost::boost_current_thread;
use stats::{PUBLISH_INTERVAL, Recorder};

use crate::{
    aim::{Aim, AimSettings},
    channel::{Message, Receiver, Sender, channel},
    clock::{self, Clock, Wait},
    output::Output,
    pipeline::{Pipeline, init_thread},
    settings::{self, Settings},
};

const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(100);
const CAPTURE_SPIN: Duration = Duration::from_millis(1);

#[derive(Debug)]
pub enum Command {
    Settings(Settings),
    LoadModel(String),
    SelectDisplay(settings::Display),
    Hold(bool),
    RunOnce,
    ReconnectOutput,
    RescanDisplays,
    RetryCapture,
    Shutdown,
}

#[derive(Debug)]
pub struct Engine {
    commands: Sender<Command>,
    join: Option<JoinHandle<()>>,
}

impl Engine {
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
                let _boost = boost_current_thread("engine");
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

/// A frame whose capture started before `started` is stale.
#[derive(Debug, Clone, Copy)]
struct ActiveRun {
    started: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Shutdown,
}

struct Worker {
    ui: Sender<Message>,
    wait: Wait,
    settings: Settings,
    state: State,
    state_changed: bool,
    recorder: Recorder,
    clock: Clock,
    stats_sent: Instant,
    pipeline: Option<Pipeline>,
    aim: Aim,
    output: Output,
    run: Option<ActiveRun>,
    next_capture: Instant,
    rebuilt: bool,
    com_ready: bool,
}

impl Worker {
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
            clock,
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
            worker.choose_startup_display();
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
                    self.publish();
                    continue;
                }
                _ => {}
            }
            self.publish();
            self.wait.until(Some(deadline), spin);
        }
    }

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

    fn begin_run(&mut self, mode: RunPhase) {
        self.aim.reset();
        let now = Instant::now();
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

    fn edit(&mut self, edit: impl FnOnce(&mut State)) {
        edit(&mut self.state);
        self.state_changed = true;
    }

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

fn chosen_port(port: Option<&str>) -> Option<String> {
    port.filter(|port| !port.trim().is_empty())
        .map(str::to_owned)
}

fn aim_settings(settings: &Settings) -> AimSettings {
    AimSettings {
        sensitivity_x: settings.sensitivity_x,
        sensitivity_y: settings.sensitivity_y,
        target_y_offset: settings.target_y_offset,
        max_delta: settings.max_delta,
        lead_mode: settings.lead_mode,
        lead: settings.lead,
        lead_extra: settings.lead_extra,
        velocity_smoothing: settings.velocity_smoothing,
    }
}

#[cfg(test)]
mod fixture {
    use super::*;
    use crate::{clock::Wake, output::fake::FakeMakcu};

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
            clock,
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
        let before = worker.aim.update(Some(detection), (100, 100), 0.0).exact;
        worker.aim.reset();

        let mut settings = worker.settings.clone();
        settings.sensitivity_x = Sensitivity::new(Sensitivity::MAX);
        worker.handle(Command::Settings(settings));

        worker.aim.reset();
        let after = worker.aim.update(Some(detection), (100, 100), 0.0).exact;
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
