# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

`nice-aimassist` is a Windows-only Rust + egui app: DXGI Desktop Duplication screen capture → GPU preprocess → YOLO ONNX inference (ONNX Runtime + DirectML) → mouse output through the vendored MAK SDK (`vendor/makxd`). The README (Japanese) is a minimal summary of user-facing behavior; trust `src/` for details.

## Commands

Windows + MSVC toolchain (all deps are under `cfg(windows)`).

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test                     # single test: cargo test <name>
cargo build --release
```

VS Code task `cargo: verify` runs fmt → clippy → test. Judge performance only on release builds. Tests use `makxd` with its `mock` feature (dev-dependency), so no hardware is needed.

Runtime data (`settings.json`, `models/`, `logs/`) lives next to the exe, or in `NICE_AIMASSIST_DATA_DIR` if set (the VS Code debug config sets it to the workspace root). `settings.json` has no version field and no migration: missing fields take their defaults (each field, so adding a setting keeps the rest), while unknown fields or out-of-range values make the whole file fall back to defaults.

## Architecture

The UI thread (`app/`) talks to one background engine over channels (`engine/mod.rs`: `EngineEvent`s out, `Command`s in via `EngineControl::send`). The engine (`engine/worker/`) owns DXGI capture, the DirectML pipeline and `MakClient` directly. Each frame runs serially: acquire → GPU copy/preprocess → inference/postprocess → output → next acquire. There are no capture/inference worker threads or frame mailbox.

- `engine/worker/frame.rs` implements the synchronous frame transaction and capture statistics. Pending commands are drained before acquisition and after inference, before output. A stop, shutdown or pipeline replacement suppresses the old result.
- `graphics/shared_frame.rs` owns one D3D11-authored texture opened by D3D12, a shared capture-ready fence and a D3D12 preprocess-completion fence. `graphics/preprocess.rs` records timed/untimed dispatch lists for the single source.
- `inference/pipeline.rs` owns the DirectML session, preprocess resources and D3D12 device. It checks GPU completion before the shared texture can be overwritten. Drop waits for the compute queue before releasing resources.

Key cross-file invariants:

- **Frames never go to the CPU.** Frame metadata (`graphics/frame.rs`) is passed by value inside the engine. No lease, slot state machine, reservation, overwrite or reclaim exists. D3D11 copy → D3D12 preprocess synchronization and completion checks remain mandatory even on a single CPU thread. A private GPU snapshot retains the previous image for reuse, with its original present timestamp.
- **The capture adapter is the single GPU identity.** Inference must use the D3D12 device on the same adapter (`AdapterLuid`). On display/adapter change, the run is paused and a candidate pipeline (capture + inference + model) is synchronously prepared and committed only on full success; on failure the previous working resources are kept. Continuous resumes with the committed or retained display pipeline under a new `RunGeneration`. Model changes leave execution stopped on both success and failure. Same rule for model loading: a failed load keeps the previous model, and only successful loads are written to config.
- **Model selection** is by file name from `models/` only (no file dialog, no absolute paths in settings).
- **Hotkeys** (`hotkeys/`): each action may have multiple shortcuts; Continuous runs while *any* Hold shortcut is down. Global dispatch is paused while capturing a new binding. XInput polling lives in `hotkeys/xinput.rs`.
- **Status to the UI** (`engine/telemetry.rs`, `StatusHub`): the engine edits its own `EngineStatus` without locking and publishes snapshots (and repaint requests) at most every ~33 ms. Never lock the shared status from the frame path; heavy fields (EP assignment) are `Arc`s.
- **Scheduling** (`platform.rs`): EcoQoS off for the process; the engine thread gets higher priority + MMCSS; the D3D12 compute queue uses NORMAL priority and the D3D11 capture device uses its default GPU thread priority. `low_latency` (settings) makes DirectML busy-wait and is applied by reloading the model.
- **Model validation** (`inference/directml/session.rs`): at load, the output must be a static `[1, N, 6]` end-to-end tensor and Ultralytics metadata (`dynamic`, `end2end`/`nms`, `imgsz`) must not contradict it; per-frame shape checks are gone, so keep this validation authoritative.
- **Settings types** (`settings.rs`, `config.rs`): Sensitivity is an integer percent (gain = sensitivity/100), target Y offset is the aim point inside the target box (0% top, 50% center, 100% bottom) applied *before* sensitivity scaling, max delta caps the length of the final (post-sensitivity) movement vector per frame (0 = unlimited), FPS limit bounds capture start cadence only in Continuous (not `Run once`). Wait through the command receiver, never a blocking sleep; set the next deadline from the actual start, never accumulate catch-up credit. No-image polls share this cadence. DXGI acquire and synchronous inference cannot be interrupted, so stop latency includes the current stage duration.
- **MAK** (`mak.rs`): mouse output only (controller output was removed; XInput remains as a hotkey source). The engine owns the client directly; `OutputStage::emit` receives `&mut MakClient`. Stop the run before reconnecting. No shared client mutex or worker acknowledgement is needed.
- **Overlay** (`overlay/`): its own thread owns a click-through, topmost, `WDA_EXCLUDEFROMCAPTURE` window (excluded so Desktop Duplication never feeds the boxes back to the model) with a DirectComposition + flip swap chain drawn by Direct2D on a private D3D11 device. The window is the model input region (`graphics::center_crop_origin`, shared with the capture copy) plus a fixed margin, so boxes stay in model input coordinates. The engine thread only calls `OverlayPublisher::publish` (a tiny mutex + thread-message wake-up, never waits on drawing); the engine thread owns `Overlay` and updates the region on every pipeline commit. Boxes expire after 1 s. Exclusive fullscreen is unsupported (borderless/windowed-fullscreen only). Overlay failures are logged and never stop the engine.
- **Tracking diagnostics** (`engine/frame_trace.rs`, `loop_metrics.rs`, `target_track.rs`): with `diagnostics.frame_trace` on, each run writes `logs/trace-<unix_ms>.jsonl` (header, one `frame` line per processed frame, footer). The engine thread only `try_send`s records; a separate thread formats and writes them. Loop metrics (period, RMS error, overshoots, convergence) are computed on the engine thread from completed frame values. The selected target is associated across frames by IoU; `OutputStage` carries the rounding remainder to the next frame of the same target and clears it on switch, loss, or run start.
- **Reuse**: invalidate the cache on stop/start, capture policy changes, pipeline replacement, expiry and DXGI AccessLost. Only Continuous may reuse images. Check expiry before inference and immediately before device output; reuse never renews the source timestamp.
- **Unsafe** is meant to stay confined (e.g. `inference/directml/native.rs`).
- Metrics (`metrics.rs`): the first inference is a cold run kept out of the rolling 256-sample stats.
