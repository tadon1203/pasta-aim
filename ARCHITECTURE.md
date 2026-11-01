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

- **UI thread** (`src/bin/app`): egui (eframe). It shows the settings and the engine state, and it writes the settings file. It holds no capture, inference, or device resource.
- **Engine thread** (`src/engine`): it owns the pipeline (`src/pipeline`), the aim (`src/aim`), and the output (`src/output`), which holds the MAKCU link. The pipeline is the DXGI Desktop Duplication capture (Direct3D 11), the GPU preprocess (Direct3D 12), and the ONNX Runtime session with DirectML. The engine thread processes one frame at a time. It also makes the statistics of the run.
- **Hotkeys thread** (`src/hotkeys`): keyboard and XInput polling, read every millisecond. It turns a shortcut into an engine command, and it reports the recorded shortcut and the controller slot to the UI. It holds no run state of the engine.

A thread takes its input from one `Receiver`. Any thread sends to it through a `Sender` (`src/channel.rs`), which also tells the receiving thread: a wake-up for the engine and hotkeys threads, a repaint for the UI. What goes into a thread is a `Command`. What goes to the UI is a `Message`.

Data flow:

- **UI thread and Hotkeys thread → Engine thread:** `engine::Command`.
- **UI thread → Hotkeys thread:** `hotkeys::Command` (the bindings, and start and cancel of a recording).
- **Engine thread and Hotkeys thread → UI thread:** `Message`. The engine sends its state when it changes, the statistics at a limited rate (see [docs/requirements.md](./docs/requirements.md)), and the success of a model load or of a display change. The Hotkeys thread sends a recorded shortcut, a cancelled recording, and the controller slot.
- **Inside the engine thread, for each frame:** the pipeline processes the next frame (acquire → GPU copy and preprocess → inference and postprocess) → the engine handles pending commands → output → next frame.
- **On the GPU:** Direct3D 11 copies the captured image into one shared texture. Direct3D 12 opens the same texture for the preprocess. Fences order the two devices.

## Code map

| Directory           | What it holds                                                                               |
| ------------------- | ------------------------------------------------------------------------------------------- |
| `src/bin/app`       | The UI: the settings panels, the device panel, and the diagnostics panel.                   |
| `src/engine`        | The engine thread: the commands, the thread loop, the runs, model and display changes, the output lifecycle, the frame step and pacing, the engine state, and the statistics. |
| `src/pipeline`      | The pipeline: one capture attempt to one postprocessed frame, and its replacement as a unit. It holds the GPU layer, the capture, the GPU preprocess, and the inference with its postprocess. |
| `src/hotkeys`       | Shortcuts: the key and controller tables, polled dispatch, recording a new shortcut, and XInput. |
| `src/aim`           | The aim: the target tracker, the move that each frame plans, and the rounding to whole counts. It knows nothing about MAKCU. |
| `src/output`        | The output: the MAKCU link (the wire protocol, the serial port, and the connect) and the write of a move. |
| `src`               | The library crate root. Also holds the small modules that serve many parts (channels and the UI message, settings, logging, and the clock with its deadline wait). |

## Invariants

- The image of a frame never goes to the CPU. Only the frame metadata moves inside the engine, by value.
- The engine processes one frame at a time. No acquire starts before the output of the frame before it returns. There is no capture thread, no inference thread, and no frame queue.
- A stop, a shutdown, or a pipeline replacement that arrives during a frame suppresses the output of that frame.
- A pipeline checks that the GPU completed its work on the shared texture before the capture can write to it again. A pipeline waits for its GPU work before it releases its resources.
- The capture adapter is the single GPU identity. Inference runs on the same adapter as the capture.
- A pipeline is replaced as a unit. The engine commits a new pipeline only when every part of it is ready. If one part fails, the engine keeps the pipeline that it had.
- After a display change, the run stays stopped, on success and on failure. After a model change, the engine stays stopped, on success and on failure.
- A failed model load keeps the model that was active. Only a successful load is written to the settings file.
- A model and a display are validated one time: the model when it loads, the captured image when the duplication is created. The frame path checks no shape, size, or format.
- A model is a file name in `models/`. The settings hold no path.
- The engine never reuses a frame. A capture attempt without a new desktop update does no inference and sends no move.
- The engine thread waits only for a command or for its capture deadline. It does not sleep on the frame path. It sets the next capture time from the start of the capture that got the new frame, and it gives no credit for lost time.
- The engine thread shares no data with another thread. It sends its state only when the state changes, and the statistics at most every 100 ms.
- Only the engine thread uses the MAKCU link. The engine stops the run before it reconnects MAKCU.
- The MAKCU link has no thread of its own. The engine thread waits for a MAKCU reply only in a connect and in the Move interpolation request, for at most 100 ms for each request. A move is one write with no reply. Every end of the link (a failed write, a missing reply, or the device going away) is reported to the engine as a lost MAKCU.
- The engine thread writes a move to the MAKCU port itself. No thread or queue sits between the move and the port.
- The remainder stays inside one track. A target switch, a lost target, and the start of a run clear it.
- Global shortcut dispatch stops while the user records a new shortcut.
