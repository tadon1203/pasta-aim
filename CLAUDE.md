# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

`nice-aimassist` is a Windows-only Rust + egui app: DXGI Desktop Duplication screen capture → GPU preprocess → YOLO ONNX inference (ONNX Runtime + DirectML) → mouse/controller output through the vendored MAK SDK (`vendor/makxd`). The README (Japanese) is a minimal summary of user-facing behavior; trust `src/` for details.

## Commands

Windows + MSVC toolchain (all deps are under `cfg(windows)`).

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test                     # single test: cargo test <name>
cargo build --release
```

VS Code task `cargo: verify` runs fmt → clippy → test. Judge performance only on release builds. Tests use `makxd` with its `mock` feature (dev-dependency), so no hardware is needed.

Runtime data (`settings.json`, `models/`, `logs/`) lives next to the exe, or in `NICE_AIMASSIST_DATA_DIR` if set (the VS Code debug config sets it to the workspace root). `settings.json` accepts only `version = 2`; older formats fall back to defaults (no backward compat by design).

## Architecture

The UI thread (`app/`) talks to a background engine over channels (`engine/mod.rs`: `EngineEvent`s out, `Command`s in via `EngineControl::send`). The engine thread (`engine/worker/`, split into run/capture/model/mak/recovery) is a state machine that owns two worker threads per pipeline and the shared `MakClient`; it aggregates telemetry and reacts to control events but is **not** on the per-frame path:

- **capture worker** (`engine/capture_worker.rs`) owns the DXGI capture device (`graphics/capture.rs`), copies frames on-GPU into a shared texture pool, and runs the BGRA→FP32/FP16 NCHW compute-shader preprocess (`graphics/preprocess.rs`, one pre-recorded command list per frame slot).
- **inference worker** (`engine/inference_worker.rs`) owns the D3D12/DirectML device and ORT session (`inference/directml/`), runs inference via `inference/pipeline.rs`, postprocesses (`inference/postprocess.rs`: nearest target selection, confidence threshold), then computes and submits the output itself through `engine/output.rs` (`OutputStage`) and `mak.rs`, and reports each frame to the engine thread as a `FrameReport`.

Key cross-file invariants:

- **Frames never go to the CPU.** Capture hands frames to inference through `graphics/frame_mailbox.rs` (`FramePublisher`/`FrameConsumer`) over `graphics/shared_frame_pool.rs`: 3 slots (`GPU_FRAME_SLOTS`) with a slot state machine and two fences (capture-ready / reuse). Drops and faults are counted in `graphics/gpu_diagnostics.rs`.
- **The capture adapter is the single GPU identity.** Inference must use the D3D12 device on the same adapter (`AdapterLuid`). On display/adapter change, a whole new pipeline (capture + inference + model) is "shadow prepared" and only committed (bumping `RunGeneration`) on full success; on failure the previous working state is kept. Same rule for model loading: a failed load keeps the previous model, and only successful loads are written to config.
- **Model selection** is by file name from `models/` only (no file dialog, no absolute paths in settings).
- **Hotkeys** (`hotkeys/`): each action may have multiple shortcuts; Continuous runs while *any* Hold shortcut is down. Global dispatch is paused while capturing a new binding. XInput polling lives in `hotkeys/xinput.rs`.
- **Causal gate** (`engine/output.rs`, `MotionGate`): frames presented at or before the last completed output are dropped by the capture thread (before the GPU copy) and again by the inference thread (before inference). It is one atomic shared by both, reset when a run starts or the injection API changes.
- **Status to the UI** (`engine/telemetry.rs`, `StatusHub`): the engine edits its own `EngineStatus` without locking and publishes snapshots (and repaint requests) at most every ~33 ms. Never lock the shared status from the frame path; heavy fields (EP assignment) are `Arc`s.
- **Scheduling** (`platform.rs`): EcoQoS off for the process; capture/inference threads get higher priority + MMCSS; the D3D12 compute queue asks for GLOBAL_REALTIME and falls back to HIGH; the D3D11 capture device raises its GPU thread priority. `low_latency` (settings) makes DirectML busy-wait and is applied by reloading the model.
- **Model validation** (`inference/directml/session.rs`): at load, the output must be a static `[1, N, 6]` end-to-end tensor and Ultralytics metadata (`dynamic`, `end2end`/`nms`, `imgsz`) must not contradict it; per-frame shape checks are gone, so keep this validation authoritative.
- **Settings types** (`settings.rs`, `config.rs`): Sensitivity is an integer percent (gain = sensitivity/100), target Y offset is a percent of target height added *after* sensitivity scaling, FPS limit applies only to Continuous (not `Run once`) and must wait via the worker command loop rather than a blocking sleep.
- **MAK** (`mak.rs`): mouse vs. controller route selection is a user choice and must not silently switch when a route disappears; "physical controller" status is separate from route status and may be "unconfirmed" rather than disconnected. The client is shared as `Arc<Mutex<MakClient>>` between the engine thread (routes, probing, release) and the inference thread (output); stop the run (which waits for the inference thread) before releasing the controller.
- **Unsafe** is meant to stay confined (e.g. `inference/directml/native.rs`).
- Metrics (`metrics.rs`): the first inference is a cold run kept out of the rolling 256-sample stats.
