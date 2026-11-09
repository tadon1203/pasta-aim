# nice-aimassist

A Windows app that captures the center of a display, finds targets with a YOLO model, and moves the mouse through a MAKCU device.

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
  {A model you want to use}.onnx
logs/
```

Put at least one `.onnx` model in `models/`. The model requirements are in [requirements.md](./docs/requirements.md#model).

## Development

Use Windows. `rust-toolchain.toml` selects the stable MSVC toolchain, and nothing else is needed.

```text
cargo run
```

The checks to run are in [CLAUDE.md](./CLAUDE.md#commands).

## Stack

- UI: egui (eframe)
- Capture: DXGI Desktop Duplication
- Inference: ONNX Runtime with DirectML
- Output: MAKCU (USB serial)

