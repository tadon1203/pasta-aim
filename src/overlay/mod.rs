//! Bounding-box overlay drawn over the model input region.
//!
//! A dedicated thread owns a click-through, capture-excluded window and its GPU
//! resources. The engine thread only writes the latest box into a small
//! shared state and posts a wake-up; it never waits for drawing, so the overlay
//! cannot slow the frame path down. Box coordinates are in model input space
//! and the window is the input region plus a margin, so no conversion is needed
//! beyond that fixed offset.

mod layout;
mod renderer;
mod window;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};

pub use layout::{BoxRect, Region};

use self::{
    layout::strokes,
    renderer::{Palette, Renderer},
    window::OverlayWindow,
};
use crate::{settings::OverlayConfig, sync::MutexExt};

/// A published box disappears after this long without a refresh, so a stalled
/// pipeline (or a finished one-shot run) does not leave a stale box behind.
const BOX_TTL: Duration = Duration::from_secs(1);

struct State {
    config: OverlayConfig,
    region: Option<Region>,
    target: Option<(BoxRect, Instant)>,
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    /// A wake-up is already queued for the overlay thread.
    wake_pending: AtomicBool,
}

/// Cloneable access to the shared state plus the means to wake the thread.
#[derive(Clone)]
struct Link {
    shared: Arc<Shared>,
    thread_id: u32,
}

impl Link {
    fn wake(&self) {
        if !self.shared.wake_pending.swap(true, Ordering::AcqRel) {
            window::post_wake(self.thread_id);
        }
    }

    fn update(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.shared.state.lock_recover());
        self.wake();
    }
}

/// Sends boxes to the overlay from the engine thread.
#[derive(Clone)]
pub struct OverlayPublisher {
    link: Link,
}

impl OverlayPublisher {
    /// Shows `target` (or removes the box on `None`). Never blocks on drawing.
    pub fn publish(&self, target: Option<BoxRect>) {
        let changed = {
            let mut state = self.link.shared.state.lock_recover();
            let previous = state.target.map(|(rect, _)| rect);
            state.target = target.map(|rect| (rect, Instant::now()));
            previous != target
        };
        // A refresh of the same box only extends its lifetime; the overlay
        // re-reads the timestamp when its timer fires.
        if changed {
            self.link.wake();
        }
    }
}

/// Owns the overlay thread.
pub struct Overlay {
    link: Link,
    join: Option<JoinHandle<()>>,
}

impl Overlay {
    pub fn spawn(config: OverlayConfig) -> Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                config,
                region: None,
                target: None,
                shutdown: false,
            }),
            wake_pending: AtomicBool::new(false),
        });
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        let join = thread::Builder::new()
            .name("nice-aimassist-overlay".to_owned())
            .spawn(move || run(thread_shared, ready_tx))
            .context("failed to spawn the overlay thread")?;
        let thread_id = ready_rx
            .recv()
            .map_err(|_| anyhow!("the overlay thread exited during startup"))?
            .map_err(|error| anyhow!(error))?;
        Ok(Self {
            link: Link { shared, thread_id },
            join: Some(join),
        })
    }

    pub fn publisher(&self) -> OverlayPublisher {
        OverlayPublisher {
            link: self.link.clone(),
        }
    }

    pub fn set_config(&self, config: OverlayConfig) {
        self.link.update(|state| state.config = config);
    }

    /// Where the model input region is on the desktop; `None` while there is
    /// no pipeline to draw for.
    pub fn set_region(&self, region: Option<Region>) {
        self.link.update(|state| {
            state.region = region;
            state.target = None;
        });
    }

    /// Removes the current box.
    pub fn clear(&self) {
        self.link.update(|state| state.target = None);
    }

    pub fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            self.link.update(|state| state.shutdown = true);
            let _ = join.join();
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// What the overlay thread needs for one pass, copied out of the lock.
struct Snapshot {
    config: OverlayConfig,
    region: Option<Region>,
    target: Option<BoxRect>,
    /// How long until the current box expires.
    timeout: Option<Duration>,
}

fn snapshot(state: &mut State, now: Instant) -> Snapshot {
    if let Some((_, published)) = state.target
        && now.duration_since(published) >= BOX_TTL
    {
        state.target = None;
    }
    Snapshot {
        config: state.config,
        region: state.region,
        target: state.target.map(|(rect, _)| rect),
        timeout: state
            .target
            .map(|(_, published)| BOX_TTL.saturating_sub(now.duration_since(published))),
    }
}

fn run(shared: Arc<Shared>, ready: mpsc::Sender<Result<u32, String>>) {
    let _init = match window::ThreadInit::new() {
        Ok(init) => init,
        Err(error) => {
            let _ = ready.send(Err(format!("{error:#}")));
            return;
        }
    };
    let _ = ready.send(Ok(window::current_thread_id()));
    let mut view = View::default();
    loop {
        // Cleared before reading, so an update made after the read re-posts.
        shared.wake_pending.store(false, Ordering::Release);
        let snapshot = {
            let mut state = shared.state.lock_recover();
            if state.shutdown {
                break;
            }
            snapshot(&mut state, Instant::now())
        };
        view.apply(&snapshot);
        window::wait_and_pump(snapshot.timeout);
    }
}

/// The overlay's window and what it currently shows.
#[derive(Default)]
struct View {
    surface: Option<(OverlayWindow, Renderer)>,
    visible: bool,
    placed: Option<Region>,
    drawn: Option<(Option<BoxRect>, OverlayConfig)>,
    /// Region for which creating the window failed; not retried until it changes.
    failed_region: Option<Region>,
}

impl View {
    fn apply(&mut self, snapshot: &Snapshot) {
        let Some(region) = snapshot.region.filter(|_| snapshot.config.enabled) else {
            self.hide();
            self.failed_region = None;
            return;
        };
        if self.failed_region == Some(region) {
            return;
        }
        if let Err(error) = self.show(snapshot, region) {
            tracing::error!(error = %format!("{error:#}"), "overlay disabled after a failure");
            self.surface = None;
            self.visible = false;
            self.placed = None;
            self.drawn = None;
            self.failed_region = Some(region);
        }
    }

    fn hide(&mut self) {
        if self.visible
            && let Some((window, _)) = &self.surface
        {
            window.set_visible(false);
        }
        self.visible = false;
    }

    fn show(&mut self, snapshot: &Snapshot, region: Region) -> Result<()> {
        let rect = region.window_rect();
        let size = (rect.2, rect.3);
        if self.surface.is_none() {
            let window = OverlayWindow::create(rect)?;
            let renderer = Renderer::new(window.hwnd(), size)?;
            tracing::info!(?region, "overlay window created");
            self.surface = Some((window, renderer));
        }
        let (window, renderer) = self.surface.as_mut().expect("created above");
        if self.placed != Some(region) {
            window.place(rect)?;
            renderer.resize(size)?;
            self.placed = Some(region);
            self.drawn = None;
        }
        let wanted = (snapshot.target, snapshot.config);
        if self.drawn != Some(wanted) {
            let config = snapshot.config;
            let shapes = snapshot
                .target
                .map(|target| {
                    strokes(
                        target,
                        config.line_width.value(),
                        config.outline_width.value(),
                    )
                })
                .unwrap_or_default();
            renderer.draw(
                &shapes,
                Palette {
                    line: config.color,
                    outline: config.outline_color,
                    opacity: config.opacity.ratio(),
                },
            )?;
            self.drawn = Some(wanted);
        }
        if !self.visible {
            window.set_visible(true);
            self.visible = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: f32) -> BoxRect {
        BoxRect {
            left,
            top: 0.0,
            right: left + 10.0,
            bottom: 10.0,
        }
    }

    fn state() -> State {
        State {
            config: OverlayConfig::default(),
            region: None,
            target: None,
            shutdown: false,
        }
    }

    #[test]
    fn a_box_expires_after_its_ttl_and_reports_the_remaining_time() {
        let start = Instant::now();
        let mut state = state();
        state.target = Some((rect(1.0), start));

        let fresh = snapshot(&mut state, start + Duration::from_millis(400));
        assert_eq!(fresh.target, Some(rect(1.0)));
        assert_eq!(fresh.timeout, Some(Duration::from_millis(600)));

        let stale = snapshot(&mut state, start + BOX_TTL);
        assert_eq!(stale.target, None);
        assert_eq!(stale.timeout, None);
        assert!(state.target.is_none());
    }

    /// Exercises the real window, swap chain and Direct2D calls end to end.
    /// The overlay is excluded from capture, so this can only be judged by eye.
    #[test]
    #[ignore = "needs an interactive desktop session and a GPU"]
    fn window_and_renderer_draw_boxes() {
        let _init = window::ThreadInit::new().unwrap();
        let region = Region {
            left: 200,
            top: 200,
            width: 400,
            height: 300,
        };
        let rect = region.window_rect();
        let window = OverlayWindow::create(rect).unwrap();
        let mut renderer = Renderer::new(window.hwnd(), (rect.2, rect.3)).unwrap();
        let palette = Palette {
            line: [255, 64, 64],
            outline: [0, 0, 0],
            opacity: 0.8,
        };
        let target = BoxRect {
            left: 50.0,
            top: 40.0,
            right: 220.0,
            bottom: 180.0,
        };
        renderer.draw(&strokes(target, 3, 2), palette).unwrap();
        window.place(rect).unwrap();
        window.set_visible(true);
        window::wait_and_pump(Some(Duration::from_millis(300)));

        // Region change: window moves and the swap chain resizes.
        let bigger = Region {
            width: 640,
            height: 640,
            ..region
        };
        let rect = bigger.window_rect();
        window.place(rect).unwrap();
        renderer.resize((rect.2, rect.3)).unwrap();
        renderer.draw(&strokes(target, 8, 4), palette).unwrap();
        window::wait_and_pump(Some(Duration::from_millis(300)));

        renderer.draw(&[None; 3], palette).unwrap();
    }

    #[test]
    #[ignore = "needs an interactive desktop session and a GPU"]
    fn overlay_thread_shows_and_clears_a_box() {
        let overlay = Overlay::spawn(OverlayConfig {
            enabled: true,
            ..OverlayConfig::default()
        })
        .unwrap();
        overlay.set_region(Some(Region {
            left: 300,
            top: 300,
            width: 320,
            height: 320,
        }));
        let publisher = overlay.publisher();
        for step in 0..30 {
            publisher.publish(Some(BoxRect {
                left: 20.0 + step as f32 * 4.0,
                top: 60.0,
                right: 120.0 + step as f32 * 4.0,
                bottom: 200.0,
            }));
            thread::sleep(Duration::from_millis(16));
        }
        overlay.clear();
        thread::sleep(Duration::from_millis(100));
    }

    #[test]
    fn publishing_the_same_box_does_not_request_another_wake() {
        // Thread id 0 never receives anything; only the pending flag matters.
        let shared = Arc::new(Shared {
            state: Mutex::new(state()),
            wake_pending: AtomicBool::new(false),
        });
        let publisher = OverlayPublisher {
            link: Link {
                shared: Arc::clone(&shared),
                thread_id: 0,
            },
        };

        publisher.publish(Some(rect(1.0)));
        assert!(shared.wake_pending.swap(false, Ordering::AcqRel));

        publisher.publish(Some(rect(1.0)));
        assert!(!shared.wake_pending.load(Ordering::Acquire));

        publisher.publish(Some(rect(2.0)));
        assert!(shared.wake_pending.swap(false, Ordering::AcqRel));

        publisher.publish(None);
        assert!(shared.wake_pending.swap(false, Ordering::AcqRel));
        publisher.publish(None);
        assert!(!shared.wake_pending.load(Ordering::Acquire));
    }
}
