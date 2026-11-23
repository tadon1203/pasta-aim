# PastaAim

A Windows app that captures the center of a display, finds targets with an end-to-end YOLO model, and moves the mouse through a MAKCU device.

See [AGENTS.md](./AGENTS.md) for the architecture and rules. See [docs/makcu-protocol.md](./docs/makcu-protocol.md) for the MAKCU wire protocol.

## Requirements

- Windows 10 or 11, x64, with a GPU that supports DirectX 12 and DirectML
- A MAKCU device connected over USB (CH340 serial chip)
- An end-to-end ONNX model (see [Model](#model))
- The .NET 10 SDK, to build from source

## Run

1. Publish the app (see [Development](#development)).
2. Put one `.onnx` model in `models/` next to the executable.
3. Start the app once to create `settings.json`. Set `detect.model` to the model's file name.
4. Start the app. Hold the hotkey (default `F8`) to correct the mouse.

The app re-reads `settings.json` when it changes. `detect.model`, `display.adapter`, and `display.output` take effect after a restart.

While the keys are down, the sample feature saves one full display image every 2 seconds to `samples/original/`. Set `samples.enabled` to `false` to turn it off.

## Settings

A missing section takes its defaults. The app clamps values that are out of range. It ignores unknown fields. If the file cannot be parsed, the app uses the defaults and leaves the file unchanged.

| Key | Default | Meaning |
| --- | --- | --- |
| `display.adapter` | `0` | DirectX adapter for capture. |
| `display.output` | `0` | Display output for capture. |
| `detect.model` | none | `.onnx` file in `models/`. Required. |
| `detect.threshold` | `50` | Minimum score threshold, in percent. |
| `correction.sensitivity_x` | `100` | Horizontal gain. |
| `correction.sensitivity_y` | `100` | Vertical gain. |
| `correction.offset` | `50` | Correction point in percent of the box height. `0` is the top. |
| `correction.max_delta` | `512` | Cap on one move. `0` is no cap. |
| `correction.lead` | `true` | Lead a moving target. |
| `correction.lead_smoothing_ms` | `12` | Lead. Smoothing time of the velocity. A larger value is steadier but slower. |
| `correction.lead_scale` | `100` | Lead. Mouse counts for one capture pixel, in percent. |
| `mouse.smoothing` | `"auto"` | MAKCU move smoothing: `"auto"` or a percent. |
| `hotkey.keys` | `["F8"]` | Keys that enable correction. |
| `samples.enabled` | `true` | Record while the keys are down. |
| `log.level` | `Info` | Most detailed log level. |

Key names are case-insensitive. Examples: `F8`, `KeyA`, `Digit1`, `Space`, `MouseLeft`, `MouseX1`, `xinput:a`, `xinput:lb`. The full list is in `Pstm/Hotkey/Key.cs`.

## Model

The model is an ONNX file with:

- A static NCHW input `[1, 3, H, W]`, in FP16 or FP32.
- A static end-to-end output `[1, N, 6]` with the columns `x1, y1, x2, y2, score, class`.
- Metadata that does not set `dynamic=True`, and that has NMS embedded (`end2end=True` or `nms=True`).

The app checks these at startup and reports the first one that fails.

## Troubleshooting

Each run writes `logs/pstm-yyyyMMdd-HHmmss.log`. The app keeps the last 10 files. Set `log.level` to `Debug` for more detail.

- `no device port found`: the device does not use the CH340 USB IDs `1A86:55D3` or `1A86:7523`. Check the cable and the driver.
- `no model is selected` or `model file not found`: set `detect.model` to a file that exists in `models/`.
- A model check error: export the model with a static input size and embedded NMS.

## Development

Build on Windows. Warnings are errors.

```text
dotnet build PastaAim.sln -c Release
dotnet test PastaAim.sln
dotnet publish Pstm/Pstm.csproj -c Release
```

The publish output is a self-contained single file in `Pstm/bin/Release/net10.0-windows/win-x64/publish/`. The checks to run before a commit are in [AGENTS.md](./AGENTS.md#commands).
