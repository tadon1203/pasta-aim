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

The UI thread (`app/`) talks to a background engine over channels (`engine/mod.rs`: `EngineEvent`s out, commands in). `engine/worker.rs` is the state machine coordinating two threads per pipeline:

- **capture worker** (`engine/capture_worker.rs`) owns the DXGI capture device (`graphics/capture.rs`), copies frames on-GPU into a shared texture pool, and runs the BGRA→FP32 NCHW compute-shader preprocess (`graphics/preprocess.rs`).
- **inference worker** (`engine/inference_worker.rs`) owns the D3D12/DirectML device and ORT session (`inference/directml/`), runs inference via `inference/pipeline.rs`, postprocesses (`inference/postprocess.rs`: nearest target selection, confidence threshold), then submits output through `mak.rs`.

Key cross-file invariants:

- **Frames never go to the CPU.** Capture hands frames to inference through `graphics/frame_mailbox.rs` (`FramePublisher`/`FrameConsumer`) over `graphics/shared_frame_pool.rs`: 3 slots (`GPU_FRAME_SLOTS`) with a slot state machine and two fences (capture-ready / reuse). Drops and faults are counted in `graphics/gpu_diagnostics.rs`.
- **The capture adapter is the single GPU identity.** Inference must use the D3D12 device on the same adapter (`AdapterLuid`). On display/adapter change, a whole new pipeline (capture + inference + model) is "shadow prepared" and only committed (bumping `RunGeneration`) on full success; on failure the previous working state is kept. Same rule for model loading: a failed load keeps the previous model, and only successful loads are written to config.
- **Model selection** is by file name from `models/` only (no file dialog, no absolute paths in settings).
- **Hotkeys** (`hotkeys/`): each action may have multiple shortcuts; Continuous runs while *any* Hold shortcut is down. Global dispatch is paused while capturing a new binding. XInput polling lives in `hotkeys/xinput.rs`.
- **Settings types** (`settings.rs`, `config.rs`): Sensitivity is an integer percent (gain = sensitivity/100), target Y offset is a percent of target height added *after* sensitivity scaling, FPS limit applies only to Continuous (not `Run once`) and must wait via the worker command loop rather than a blocking sleep.
- **MAK** (`mak.rs`): mouse vs. controller route selection is a user choice and must not silently switch when a route disappears; "physical controller" status is separate from route status and may be "unconfirmed" rather than disconnected.
- **Unsafe** is meant to stay confined (e.g. `inference/directml/native.rs`).
- Metrics (`metrics.rs`): the first inference is a cold run kept out of the rolling 256-sample stats.
