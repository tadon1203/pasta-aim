# Coding standards

These are the rules for code and documentation that lint cannot check. Do not write a rule here if a lint or format tool enforces it. The tool configuration is the source of truth for those rules.

## All code and documentation

- Write comments and documentation in English. Follow ASD-STE100 (Simplified Technical English).
- Use the terms in [CONTEXT.md](../CONTEXT.md), not synonyms.
- Documentation is the single source of truth. Keep each fact in one file. Link; do not copy.

## Rust

- Use `unsafe` only to call a Windows, Direct3D, DirectML, or ONNX Runtime API. Do not write `unsafe` in `src/app` or `src/engine`.
- A setting that has a range is its own type. The type clamps the value when it is made, and the settings file is validated against the same range when it is loaded.
- Judge performance only on a release build.

## Tests

- A test stays in the file of the code that it tests, in `#[cfg(test)] mod tests`.
- A test name is a sentence that states the behavior.
- A test needs no hardware: no display, no GPU, and no MAKCU device. A test that needs a desktop session and a GPU has `#[ignore]` with that reason, so `cargo test` does not run it.
- Replace only MAKCU, with `FakeMakcu` at the `Connector` seam. There is no stand-in for a GPU or an operating system API.
- Keep logic out of the code that calls a GPU or an operating system API, so that a test reaches the logic without hardware.
