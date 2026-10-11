# nice-aimassist

A Windows app that captures the center of a display, finds targets with a YOLO model, and moves the mouse through a MAK device.

## Documentation

- [CLAUDE.md](./CLAUDE.md): the list of documents
- [Requirements](./docs/requirements.md): how the app behaves, with every setting
- [Architecture](./ARCHITECTURE.md): system map and engineering rules

## Run

Put the files next to the executable:

```text
nice-aimassist.exe
settings.json
models/
  yolo26n.onnx
logs/
```

Put at least one `.onnx` model in `models/`. The model requirements are in [requirements.md](./docs/requirements.md#model).

## Development

Use Windows and the MSVC Rust toolchain.

```text
cargo run
```

The checks to run are in [CLAUDE.md](./CLAUDE.md#commands).

## Stack

- UI: egui (eframe)
- Capture: DXGI Desktop Duplication
- Inference: ONNX Runtime with DirectML
- Output: the MAK SDK, vendored in `vendor/makxd`

## Agent skills

Skills are in `.claude/skills`.
