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

One Windows process, written in Rust. Each thread owns its resources, and the threads talk through channels. The terms are defined in [CONTEXT.md](./CONTEXT.md).

- **UI thread** (`src/app`): egui (eframe). It shows the settings and the engine status, and it writes the settings file. It holds no capture, inference, or device resource.
- **Engine thread** (`src/engine`): it owns the Pipeline and the MAK client. The Pipeline is the DXGI Desktop Duplication capture (Direct3D 11), the GPU preprocess (Direct3D 12), and the ONNX Runtime session with DirectML. The engine thread processes one frame at a time.
- **Input thread** (`src/hotkeys`): global keyboard hotkeys and XInput polling. It turns a Shortcut into an engine command. It holds no run state of the engine.
- **Trace thread** (`src/engine`): it formats and writes the Frame trace.

Data flow:

- **UI thread and Input thread → Engine thread:** commands, on one channel.
- **Engine thread → UI thread:** events (the result of a model load or of a capture selection) on a channel, and status snapshots that the engine publishes at a limited rate, each with a repaint request.
- **Engine thread → Trace thread:** one record for each processed frame.
- **Inside the engine thread, for each frame:** acquire → GPU copy and preprocess → inference and postprocess → output → next acquire. The engine handles pending commands before the acquire, and again after the inference and before the output.
- **On the GPU:** Direct3D 11 copies the captured image into one shared texture. Direct3D 12 opens the same texture for the preprocess. Fences order the two devices.

## Code map

| Directory           | What it holds                                                                               |
| ------------------- | ------------------------------------------------------------------------------------------- |
| `src/app`           | The UI: the settings panels, the device panel, and the diagnostics panel.                   |
| `src/engine`        | The engine thread: commands, status, output, the Target track, loop metrics, and the trace. |
| `src/engine/worker` | The frame transaction, the Run, model loading, Pipeline replacement, and the MAK lifecycle. |
| `src/graphics`      | The capture, the shared frame texture and its fences, the GPU preprocess, and GPU faults.   |
| `src/inference`     | The inference Pipeline, the postprocess, and the selection of the target.                   |
| `src/hotkeys`       | Shortcuts: parsing, global dispatch, recording a new Shortcut, and XInput.                  |
| `src`               | Also holds the small modules that serve many parts (settings, paths, logging, timing, MAK). |
| `vendor/makxd`      | The MAK SDK, vendored.                                                                      |

## Invariants

- The image of a frame never goes to the CPU. Only the frame metadata moves inside the engine, by value.
- The engine processes one frame at a time. No acquire starts before the output of the frame before it returns. There is no capture thread, no inference thread, and no frame queue.
- A stop, a shutdown, or a Pipeline replacement that arrives during a frame suppresses the output of that frame.
- The engine checks that the GPU completed its work on the shared texture before the capture can write to it again. A Pipeline waits for its GPU work before it releases its resources.
- The Capture adapter is the single GPU identity. Inference runs on the same adapter as the capture.
- A Pipeline is replaced as a unit. The engine commits a Candidate pipeline only when every part of it is ready. If one part fails, the engine keeps the Pipeline that it had.
- After a display change, a Continuous run resumes under a new Run generation. After a model change, the engine stays stopped, on success and on failure.
- A failed model load keeps the model that was active. Only a successful load is written to the settings file.
- A model is validated one time, when it loads. The frame path does not check tensor shapes, so the load-time validation must stay complete.
- A model is a file name in `models/`. The settings hold no path.
- The engine never reuses a frame. A capture attempt without a new desktop update does no inference and sends no Movement.
- The engine thread waits only on its command channel. It does not sleep on the frame path. It sets the next capture time from the actual start of the current capture, and it gives no credit for lost time.
- The engine thread does not lock the shared status on the frame path. It edits its own copy and publishes snapshots.
- The engine thread never waits for the Trace thread. A failure of the trace never stops the engine.
- Only the engine thread uses the MAK client. The engine stops the Run before it reconnects MAK.
- The Rounding remainder stays inside one Target track. A Target switch, a lost target, and the start of a Run clear it.
- Global Shortcut dispatch stops while the user records a new Shortcut.
