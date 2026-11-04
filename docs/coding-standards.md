# Coding standards

These are the rules for code and documentation that lint cannot check. Do not write a rule here if a lint or format tool enforces it. The tool configuration is the source of truth for those rules.

## All code and documentation

- Write comments and documentation in English. Follow ASD-STE100 (Simplified Technical English).
- Documentation is the single source of truth. Keep each fact in one file. Link; do not copy.

## Rust

- Modules are split by feature. A module is `foo.rs`, with `foo/` only when a child file has clear value: it hides a large part behind a small interface, or it is test-only. No split by line count. No `mod.rs`.
- Names are plain English words, the shortest that is not ambiguous. The module path gives the context: `engine::Command`, not `engine::EngineCommand`. A name that equals its module (`settings::Settings`) is allowed.
- Errors: use `thiserror` only when a caller branches on the kind of error. Use `anyhow` with `.context` everywhere else. Store a `String` only when an error is kept for display, and format it with `{error:#}`. A startup failure returns to `main`.
- The library never uses `eframe` or `egui`. The bin makes the repaint closure that it gives to `channel::channel`.
- Communication: a thread takes its input from one `Receiver<T>`, and any thread sends through a `Sender<T>`. What goes into a thread is a `Command`. What goes to the UI is a `Message`. `send` never blocks and ignores a closed receiver. A thread owner (`Engine`, `Hotkeys`) has `spawn` and `sender`, and it stops and joins its thread on drop.
- Use `unsafe` only to call a Windows, Direct3D, DirectML, or ONNX Runtime API.
- A setting that has a range is its own type. The type clamps the value when it is made, and the settings file is validated against the same range when it is loaded.
- Judge performance only on a release build.

## Tests

- A test stays in the file of the code that it tests, in `#[cfg(test)] mod tests`.
- A test name is a sentence that states the behavior.
- A test needs no hardware: no display, no GPU, and no MAKCU device. A test that needs a desktop session and a GPU has `#[ignore]` with that reason, so `cargo test` does not run it.
- Replace only MAKCU, with `FakeMakcu` at the `Connector` seam. There is no stand-in for a GPU or an operating system API.
- Keep logic out of the code that calls a GPU or an operating system API, so that a test reaches the logic without hardware.
- Keep a test only when it protects a requirement or an invariant. Do not test std, serde, or egui behavior, constants, or private fields.
