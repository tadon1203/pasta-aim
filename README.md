# nice-aimassist

A Windows app that captures the center of a display, finds targets with an end-to-end YOLO model, and moves the mouse through a MAKCU device.

## Documentation

- [CLAUDE.md](./CLAUDE.md): architecture, rules, and the list of documents
- [docs/makcu-protocol.md](./docs/makcu-protocol.md): the MAKCU wire protocol

## Requirements

- Windows 10 or 11, x64, in a desktop session, with a GPU that supports DirectX 12 and DirectML
- A MAKCU device connected over USB (CH340 serial chip)
- A YOLO model exported as an end-to-end ONNX model (see [Model](#model))
- The .NET 10 SDK, to build from source

## Run

Put the files next to the executable:

```text
nice-aimassist.exe
settings.json        created with the defaults on first start when missing
models/
  {model}.onnx
logs/                created on start; the last 10 log files are kept
dataset/             created when recording saves its first sample
```

1. Build or publish the app (see [Development](#development)).
2. Create `models/` next to the executable and put one `.onnx` model in it.
3. Set `detect.model` in `settings.json` to the model's file name, for example `"detect": { "model": "target.onnx" }`.
4. Start the app. Hold the hotkey (default `F8`) to aim.

Settings are re-read when `settings.json` changes. `detect.model`, `display.adapter` and `display.output` take effect after a restart.

### Controls

- **Hotkey** (`hotkey.keys`, default `F8`): aim while any listed key is held.
- **Dataset**: saves training samples while a hotkey is down. Off when `dataset.enabled` is `false`.

### Settings

The file has one object per feature. A missing section takes its defaults.

```json
{
  "display": { "adapter": 0, "output": 0 },
  "detect": { "model": "target.onnx", "confidence": 50 },
  "aim": {
    "sensitivity_x": 100,
    "sensitivity_y": 100,
    "target_y_offset": 50,
    "max_delta": 512,
    "velocity_smoothing": 50
  },
  "mouse": { "interpolation": "auto" },
  "hotkey": { "keys": ["F8"] },
  "dataset": { "enabled": true, "size": 640, "rate": 2 },
  "log": { "level": "Info" }
}
```

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `display.adapter` | integer | `0` | DirectX adapter LUID used for capture. Restart only. |
| `display.output` | integer | `0` | Display output index used for capture. Restart only. |
| `detect.model` | file name | none | `.onnx` file inside `models/`. Required. Restart only. |
| `detect.confidence` | 0 to 100 | `50` | Minimum detection confidence, in percent. |
| `aim.sensitivity_x` | 1 to 10000 | `100` | Horizontal gain. |
| `aim.sensitivity_y` | 1 to 10000 | `100` | Vertical gain. |
| `aim.target_y_offset` | 0 to 100 | `50` | Aim point inside the target box, in percent of its height. `0` is the top edge. |
| `aim.max_delta` | 0 to 32767 | `512` | Cap on the length of one move. `0` is no cap. |
| `aim.velocity_smoothing` | 0 to 95 | `50` | Smoothing of the target velocity estimate. |
| `mouse.interpolation` | `"auto"` or 0 to 100 | `"auto"` | MAKCU move smoothing: its Auto mode or a fixed percent. |
| `hotkey.keys` | list of key names | `["F8"]` | Keys that enable aiming. |
| `dataset.enabled` | boolean | `true` | Whether recording runs while a hotkey is down. |
| `dataset.size` | 64 to 4096 | `640` | Side of the centered square saved as the training image, in pixels. |
| `dataset.rate` | 1 to 2 | `2` | Shots saved per second while recording. |
| `log.level` | `Error`, `Warn`, `Info`, `Debug`, `Trace` | `Info` | Most detailed level written to the log file. |

Values outside a range are clamped. Unknown fields are ignored. A file that cannot be parsed gives the defaults, and the file is left unchanged. A settings file from an earlier version is not migrated. Its values are lost, and the defaults apply.

Key names are case-insensitive. Examples: `F8`, `KeyA`, `A`, `Digit1`, `Space`, `MouseLeft`, `MouseX1`, `xinput:a`, `xinput:lb`, `xinput:lt`. The full list is in `NiceAimassist/Hotkey/KeyInput.cs`.

### Model

The model must be an ONNX file with:

- A static NCHW input `[1, 3, H, W]`, in FP16 or FP32.
- A static end-to-end output `[1, N, 6]` with the columns `x1, y1, x2, y2, score, class`.
- Metadata that does not set `dynamic=True`, and that has NMS embedded (`end2end=True` or `nms=True`).

The app checks these at startup and reports the first one that fails.

### Logs and troubleshooting

Each run writes `logs/nice-aimassist-yyyyMMdd-HHmmss.log`. Set `log.level` to `Debug` for more detail.

- `no MAKCU port found`: the device is not registered under the CH340 USB IDs `1A86:55D3` or `1A86:7523`. Check the cable and the driver.
- `no model is selected` or `model file not found`: set `detect.model` to a file that exists in `models/`.
- A model check error: export the model with a static input size, with NMS embedded.

## Development

Build and publish on Windows. The projects target `net10.0-windows`, and warnings are errors.

```text
dotnet build NiceAimassist.sln -c Release
dotnet test NiceAimassist.sln
dotnet publish NiceAimassist/NiceAimassist.csproj -c Release
```

The publish output is a self-contained single file in `NiceAimassist/bin/Release/net10.0-windows/win-x64/publish/`.

The checks to run before a commit are in [CLAUDE.md](./CLAUDE.md#commands).

## Stack

- UI: Windows Forms settings window
- Capture: DXGI Desktop Duplication through Silk.NET; the center square is copied into a shared D3D12 texture
- Inference: ONNX Runtime with the DirectML execution provider
- Output: MAKCU over USB serial (`System.IO.Ports`)
- Controller hotkeys: XInput
