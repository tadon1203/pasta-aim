# Architecture

This document is the map of the system and the rules that keep it in shape.

## About this document

This document changes rarely. Keep it stable.

- The sections are fixed, as in the template below. Do not add, remove, or rename a section. Put new content in one of them.
- Do not put information here that changes often.
- Do not name a file in the Code map. Name a directory.
- Do not describe an implementation detail. The code and its comments do that.
- Change this document only when the structure or a rule changes.

The template of this document. Text in `<>` is a placeholder: replace it, and keep everything else.

```markdown
# Architecture

<What this document is.>

## About this document

<The rules for changing this document.>

## Mental model

<The technology stack, by layer: its directory, its technology, what it owns, and what it must not hold.>

<How data flows through the layers: each path, its direction, and what it carries.>

## Code map

<Where each part lives, by directory.>

| Directory     | What it holds              |
| ------------- | -------------------------- |
| `<directory>` | <What it holds, in a line> |

## Invariants

- <A rule that must never break, so the design does not decay.>
```

## Mental model

One Windows process, written in Rust. Each thread owns its resources, and the threads talk through channels. The domain terms are defined in [docs/requirements.md](./docs/requirements.md).

- **UI thread** (`src/app`): egui (eframe). It shows the settings and the engine status, and it writes the settings file. It holds no capture, inference, or device resource.
- **Engine thread** (`src/engine`): it owns the pipeline (`src/pipeline`) and the MAKCU client. The pipeline is the DXGI Desktop Duplication capture (Direct3D 11), the GPU preprocess (Direct3D 12), and the ONNX Runtime session with DirectML. The engine thread processes one frame at a time.
- **Input thread** (`src/hotkeys`): global keyboard hotkeys and XInput polling. It turns a shortcut into an engine command. It holds no run state of the engine.
- **Telemetry thread** (`src/telemetry`): it takes the frame records. It makes the statistics and the loop metrics, writes the frame trace, and publishes the live values for the UI.

Data flow:

- **UI thread and Input thread → Engine thread:** commands, on one channel.
- **Engine thread → UI thread:** events (the result of a model load or of a capture selection) on a channel, and the engine state, published when it changes.
- **Engine thread → Telemetry thread:** one frame record for each processed frame, and the counters of a run.
- **Telemetry thread → UI thread:** the live values, at a limited rate (see [docs/requirements.md](./docs/requirements.md)).
- **Inside the engine thread, for each frame:** the pipeline processes the next frame (acquire → GPU copy and preprocess → inference and postprocess) → the engine handles pending commands → output → next frame.
- **On the GPU:** Direct3D 11 copies the captured image into one shared texture. Direct3D 12 opens the same texture for the preprocess. Fences order the two devices.

## Code map

| Directory           | What it holds                                                                               |
| ------------------- | ------------------------------------------------------------------------------------------- |
| `src/app`           | The UI: the settings panels, the device panel, and the diagnostics panel.                   |
| `src/engine`        | The engine thread: commands, the engine state, output, and the track.                       |
| `src/engine/worker` | The frame transaction, the run, model loading, pipeline replacement, and the MAKCU lifecycle. |
| `src/pipeline`      | The pipeline: one capture attempt to one postprocessed frame, and its replacement as a unit. |
| `src/telemetry`     | The Telemetry thread: the frame records, the statistics, the loop metrics, the frame trace, and the live values. |
| `src/graphics`      | The capture, the shared frame texture and its fences, the GPU preprocess, and GPU faults.   |
| `src/inference`     | The inference session, the postprocess, and the selection of the target.                    |
| `src/hotkeys`       | Shortcuts: parsing, global dispatch, recording a new shortcut, and XInput.                  |
| `src/makcu`         | The MAKCU client: the wire protocol, the serial port, and the connection.                   |
| `src`               | Also holds the small modules that serve many parts (settings, paths, logging, timing, and the deadline wait). |

## Invariants

- The image of a frame never goes to the CPU. Only the frame metadata moves inside the engine, by value.
- The engine processes one frame at a time. No acquire starts before the output of the frame before it returns. There is no capture thread, no inference thread, and no frame queue.
- A stop, a shutdown, or a pipeline replacement that arrives during a frame suppresses the output of that frame.
- A pipeline checks that the GPU completed its work on the shared texture before the capture can write to it again. A pipeline waits for its GPU work before it releases its resources.
- The capture adapter is the single GPU identity. Inference runs on the same adapter as the capture.
- A pipeline is replaced as a unit. The engine commits a new pipeline only when every part of it is ready. If one part fails, the engine keeps the pipeline that it had.
- After a display change, a continuous run resumes under a new run generation. After a model change, the engine stays stopped, on success and on failure.
- A failed model load keeps the model that was active. Only a successful load is written to the settings file.
- A model and a display are validated one time: the model when it loads, the captured image when the duplication is created. The frame path checks no shape, size, or format.
- A model is a file name in `models/`. The settings hold no path.
- The engine never reuses a frame. A capture attempt without a new desktop update does no inference and sends no move.
- The engine thread waits only for a command or for its capture deadline. It does not sleep on the frame path. It sets the next capture time from the start of the capture that got the new frame, and it gives no credit for lost time.
- The engine thread does not lock shared data on the frame path. It publishes its state only when the state changes, and it sends one frame record for each processed frame.
- The engine thread never waits for the Telemetry thread between a capture and its output. A failure of the Telemetry thread never stops the engine.
- Only the engine thread uses the MAKCU client. The engine stops the run before it reconnects MAKCU.
- The engine thread writes a move to the MAKCU port itself. No thread or queue sits between the move and the port.
- The remainder stays inside one track. A target switch, a lost target, and the start of a run clear it.
- Global shortcut dispatch stops while the user records a new shortcut.
