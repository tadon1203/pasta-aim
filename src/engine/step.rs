//! One frame of the engine: the pipeline's attempt, the pending commands, the output, and the pacing.

use std::time::{Duration, Instant};

use super::{CapturePhase, Command, Flow, ModelPhase, RunPhase, Worker};
use crate::{
    channel::Receiver,
    clock::Clock,
    pipeline::{Fault, Frame},
    settings::{self, FpsLimit},
};

/// The earliest start of the next capture in a Continuous run. It counts from `started`, the start of the
/// capture that got the last new frame. A late frame gives no burst: its next capture is due at once,
/// and the capture after that counts from its own start.
pub(super) fn next_capture(started: Instant, fps: FpsLimit) -> Instant {
    started + Duration::from_secs_f64(1.0 / f64::from(fps.get()))
}

impl Worker {
    /// The start of the next capture, while a Continuous run waits for it. Run once is never paced.
    pub(super) fn pending_capture(&self, now: Instant) -> Option<Instant> {
        let continuous = self.run.is_some() && self.state.run.phase == RunPhase::Continuous;
        (continuous && now < self.next_capture).then_some(self.next_capture)
    }

    /// One capture attempt of the active run. The commands are applied before the capture and after it,
    /// so that a stop or a change suppresses the output of a frame that arrives late.
    pub(super) fn step_frame(&mut self, commands: &Receiver<Command>) -> Flow {
        if self.drain(commands) == Flow::Shutdown {
            return Flow::Shutdown;
        }
        if self.run.is_none() {
            return Flow::Continue;
        }
        let attempt_start = Instant::now();
        let wait_ms = u32::from(self.settings.new_frame_wait.get());
        let Some(pipeline) = self.pipeline.as_mut() else {
            self.fail_run("inference pipeline is not prepared".to_owned());
            return Flow::Continue;
        };
        match pipeline.process_next(wait_ms) {
            Ok(None) => Flow::Continue,
            Ok(Some(frame)) => {
                let flow = self.finish_frame(commands, attempt_start, &frame);
                // Any capture that got a new frame sets the next deadline, also when a stop suppressed its output.
                self.next_capture = next_capture(attempt_start, self.settings.fps_limit);
                flow
            }
            Err(fault) => {
                self.handle_fault(&fault);
                Flow::Continue
            }
        }
    }

    /// The part of a frame after the pipeline: the pending commands, the stop check, the output, and the
    /// statistics.
    fn finish_frame(
        &mut self,
        commands: &Receiver<Command>,
        attempt_start: Instant,
        frame: &Frame,
    ) -> Flow {
        if self.drain(commands) == Flow::Shutdown {
            return Flow::Shutdown;
        }
        // The frame is current when its run is still active and the run began before the capture.
        if self.run.is_none_or(|run| run.started > attempt_start) {
            self.recorder.frame_dropped();
            return Flow::Continue;
        }
        let output_start = Clock::now();
        let captured = self.clock.ms(0, frame.times.present) / 1000.0;
        self.aim
            .observe_delay(self.clock.ms(frame.times.present, output_start) / 1000.0);
        let outcome = self.aim.update(frame.selected, self.input_size(), captured);
        if let Some(rounded) = outcome.rounded
            && let Err(error) = self.output.write(rounded)
        {
            self.output_lost(format!("{error:#}"));
            return Flow::Continue;
        }
        let output_done = Clock::now();
        self.recorder
            .frame(frame, &outcome, output_start, output_done, Instant::now());
        self.note_output(frame.desktop_size);
        self.rebuilt = false;
        if self.state.run.phase == RunPhase::OneShot {
            self.stop_run();
        }
        Flow::Continue
    }

    /// The model input size that the output plans against. The state holds it.
    fn input_size(&self) -> (u32, u32) {
        self.state.model.input_size.unwrap_or((0, 0))
    }

    /// Stops the run and drops the pipeline. A GPU fault gets one rebuild, until a frame reaches the
    /// output. Otherwise the capture fails, and the user retries with `RetryCapture`.
    fn handle_fault(&mut self, fault: &Fault) {
        self.recorder.frame_dropped();
        let message = format!("{fault}");
        let rebuild = matches!(fault, Fault::Gpu(_)) && !self.rebuilt;
        self.fail_run(message.clone());
        // Dropping the pipeline waits for its GPU queue before its objects are released.
        self.pipeline = None;
        self.edit(|state| {
            state.model.phase = ModelPhase::Empty;
            state.capture.selected = None;
            state.capture.desktop_size = None;
        });
        let result = if rebuild {
            self.rebuilt = true;
            self.rebuild_pipeline()
        } else {
            Err(anyhow::anyhow!(message))
        };
        if let Err(error) = result {
            let message = format!("{error:#}");
            tracing::error!(%message, "pipeline is not rebuilt; capture failed");
            self.edit(|state| {
                state.capture.phase = CapturePhase::Failed;
                state.capture.error = Some(message);
            });
        }
    }

    /// Builds the pipeline again with the active model and the selected display.
    fn rebuild_pipeline(&mut self) -> anyhow::Result<()> {
        let Some(name) = self.state.model.active_name.clone() else {
            anyhow::bail!("no model is loaded");
        };
        let path = settings::model_path(&name)?;
        let pipeline = self.build_pipeline(&self.settings.display, &path)?;
        self.commit_pipeline(pipeline);
        Ok(())
    }

    /// Edits the state for a frame that reached the output. Each edit is made only when its value changes.
    fn note_output(&mut self, desktop_size: (usize, usize)) {
        if self.state.capture.desktop_size != Some(desktop_size) {
            self.edit(|state| state.capture.desktop_size = Some(desktop_size));
        }
        if self.state.run.error.is_some() {
            self.edit(|state| state.run.error = None);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::super::fixture::worker;
    use super::*;
    use crate::{
        clock::Clock,
        output::fake::FakeMakcu,
        pipeline::{Detection, Times},
        settings::{Interpolation, InterpolationPercent},
    };

    /// The Move frames that the fake MAKCU received.
    fn moves(fake: &FakeMakcu) -> Vec<Vec<u8>> {
        fake.frames()
            .into_iter()
            .filter(|frame| frame[4] == 0x18)
            .collect()
    }

    /// A new frame with a valid detection, as the pipeline would hand it to the engine.
    fn processed(present_qpc: i64) -> Frame {
        Frame {
            desktop_size: (100, 100),
            times: Times {
                present: present_qpc,
                attempt_start: present_qpc,
                acquired: present_qpc,
                copy_submitted: present_qpc,
                ready: present_qpc,
                inference_start: present_qpc,
                inference_done: present_qpc,
                postprocess_done: present_qpc,
            },
            gpu_preprocess_ms: None,
            gpu_copy_ms: None,
            selected: Some(Detection::for_test(70.0, 50.0, 0)),
        }
    }

    fn finish(worker: &mut Worker, commands: &Receiver<Command>, frame: &Frame) -> Flow {
        worker.finish_frame(commands, Instant::now(), frame)
    }

    #[test]
    fn next_capture_is_one_period_after_the_start_of_the_frame_capture() {
        let start = Instant::now();
        let fps = FpsLimit::new(100);
        assert_eq!(next_capture(start, fps), start + Duration::from_millis(10));
    }

    #[test]
    fn a_late_frame_gives_no_burst() {
        let fps = FpsLimit::new(100);
        let late = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        // The deadline counts from the late start, so it is already due once, and nothing accumulates.
        let deadline = next_capture(late, fps);
        assert_eq!(deadline, late + Duration::from_millis(10));
        assert!(deadline < Instant::now());
    }

    #[test]
    fn continuous_runs_are_paced_and_run_once_is_not() {
        let (mut worker, _fake, _messages, _waker) = worker();
        let now = Instant::now();
        worker.next_capture = now + Duration::from_secs(10);
        assert_eq!(worker.pending_capture(now), None, "no run");

        worker.begin_run(RunPhase::OneShot);
        assert_eq!(
            worker.pending_capture(now),
            None,
            "run once ignores the limit"
        );

        worker.stop_run();
        worker.begin_run(RunPhase::Continuous);
        // A run's first capture is due at once. A later deadline makes the capture wait.
        assert_eq!(worker.pending_capture(Instant::now()), None);
        let now = Instant::now();
        worker.next_capture = now + Duration::from_secs(10);
        assert_eq!(
            worker.pending_capture(now),
            Some(now + Duration::from_secs(10))
        );
    }

    #[test]
    fn stop_and_shutdown_during_the_fps_wait_are_handled_at_once() {
        let (mut worker, _fake, _messages, waker) = worker();
        worker.begin_run(RunPhase::Continuous);
        worker.next_capture = Instant::now() + Duration::from_secs(10);
        let (tx, rx) = mpsc::channel();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            tx.send(Command::Hold(false)).unwrap();
            tx.send(Command::Shutdown).unwrap();
            waker.wake();
        });
        let start = Instant::now();
        worker.event_loop(&rx);
        sender.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(worker.run.is_none());
        assert!(worker.state.run.error.is_none());
    }

    #[test]
    fn queued_stop_shutdown_and_pipeline_edits_suppress_old_output() {
        for command in [
            Command::Hold(false),
            Command::Shutdown,
            Command::LoadModel("missing.onnx".into()),
            Command::SelectDisplay(settings::Display {
                adapter: 99,
                output: 99,
            }),
        ] {
            let shutdown = matches!(command, Command::Shutdown);
            let (mut worker, fake, _messages, _waker) = worker();
            worker.begin_run(RunPhase::Continuous);
            let frame = processed(Clock::now());
            let (tx, rx) = mpsc::channel();
            tx.send(command).unwrap();
            let flow = finish(&mut worker, &rx, &frame);
            assert_eq!(flow == Flow::Shutdown, shutdown);
            assert_eq!(moves(&fake).len(), 0);
            // A stale frame is counted as dropped. A shutdown ends before the output stage.
            assert_eq!(worker.recorder.counts(), (0, u64::from(!shutdown)));
        }
    }

    #[test]
    fn a_valid_detection_reaches_the_output_and_the_statistics() {
        let (mut worker, fake, _messages, _waker) = worker();
        worker.begin_run(RunPhase::Continuous);
        let (_tx, rx) = mpsc::channel();
        let frame = processed(Clock::now());
        assert_eq!(finish(&mut worker, &rx, &frame), Flow::Continue);
        assert_ne!(moves(&fake).len(), 0);
        assert_eq!(worker.recorder.counts(), (1, 0));
        let stats = worker.recorder.stats();
        assert_eq!(
            stats.last_target.map(|target| (target.0, target.1)),
            Some((20.0, 0.0))
        );
        assert!(stats.last_move.is_some());
    }

    #[test]
    fn a_failed_move_write_is_a_lost_makcu_and_stops_the_run() {
        let (mut worker, fake, _messages, _waker) = worker();
        worker.begin_run(RunPhase::Continuous);
        fake.disconnect();
        let (_tx, rx) = mpsc::channel();
        assert_eq!(
            finish(&mut worker, &rx, &processed(Clock::now())),
            Flow::Continue
        );
        assert_eq!(
            worker.state.output.phase,
            crate::engine::OutputPhase::Failed
        );
        assert!(worker.state.output.error.is_some());
        assert!(worker.run.is_none());
        assert!(!worker.output.is_connected());
        assert_eq!(
            worker.recorder.counts().0,
            0,
            "the frame did not reach the output"
        );
    }

    #[test]
    fn the_stored_interpolation_is_sent_and_a_missing_reply_is_a_lost_makcu() {
        let (mut worker, fake, _messages, _waker) = worker();
        worker.settings.interpolation = Interpolation::Percent(InterpolationPercent::new(40));
        worker.send_interpolation();
        assert_eq!(fake.frames().last().unwrap()[4..], [0x1F, 40]);
        assert!(worker.output.is_connected());

        fake.mute_next(0x1F);
        worker.send_interpolation();
        assert_eq!(
            worker.state.output.phase,
            crate::engine::OutputPhase::Failed
        );
        assert!(!worker.output.is_connected());
    }

    #[test]
    fn a_rejected_interpolation_shows_an_error_and_keeps_the_makcu() {
        let (mut worker, fake, _messages, _waker) = worker();
        worker.settings.interpolation = Interpolation::Percent(InterpolationPercent::new(40));
        fake.reject_next(0x1F);
        worker.send_interpolation();
        assert!(worker.state.run.error.is_some());
        assert!(worker.output.is_connected());
    }

    #[test]
    fn a_failed_display_change_and_model_load_keep_the_previous_selection() {
        let (mut worker, _fake, _messages, _waker) = worker();
        worker.begin_run(RunPhase::Continuous);
        let before = worker.settings.display;
        worker.handle(Command::SelectDisplay(settings::Display {
            adapter: 99,
            output: 99,
        }));
        assert_eq!(worker.settings.display, before);
        assert!(worker.run.is_none(), "a display change stops the run");
        assert!(worker.state.capture.error.is_some());

        worker.handle(Command::LoadModel("missing.onnx".into()));
        assert!(worker.state.model.error.is_some());
        assert_eq!(worker.state.model.active_name, None);
    }

    #[test]
    fn one_shot_outputs_once_and_stops() {
        let (mut worker, fake, _messages, _waker) = worker();
        worker.begin_run(RunPhase::OneShot);
        let (_tx, rx) = mpsc::channel();
        assert_eq!(
            finish(&mut worker, &rx, &processed(Clock::now())),
            Flow::Continue
        );
        assert_ne!(moves(&fake).len(), 0);
        assert!(worker.run.is_none());
        assert_eq!(worker.state.run.phase, RunPhase::Idle);
        assert_eq!(worker.recorder.counts().0, 1);
    }

    #[test]
    fn a_frame_captured_before_a_new_run_is_dropped() {
        let (mut worker, _fake, _messages, _waker) = worker();
        let attempt_start = Instant::now();
        worker.stop_run();
        worker.begin_run(RunPhase::Continuous);
        let (_tx, rx) = mpsc::channel();
        // The run that is active now began after the capture: the frame is stale.
        assert_eq!(
            worker.finish_frame(&rx, attempt_start, &processed(Clock::now())),
            Flow::Continue
        );
        assert_eq!(worker.recorder.counts(), (0, 1));
    }
}
