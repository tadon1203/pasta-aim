# Requirements

A Windows app that captures the center of a display, finds targets with a YOLO model, and moves the mouse toward the Selected target through a MAKCU device. Low and stable latency comes before features.

The terms are defined in [CONTEXT.md](../CONTEXT.md).

## Runtime data

- `settings.json`, `models/`, and `logs/` are next to the executable.

## Model

- The user picks a model by file name from the `.onnx` files in `models/`. There is no file dialog.
- The input and the output can be FP32 or FP16. The app reads the precision from the model and shows it as `Active input`.
- The input size is fixed.
- The output is one static tensor of shape `[1, N, 6]`. Each row is `x1, y1, x2, y2, score, class`. The model is end-to-end: NMS is part of the model.
- A model is rejected when it loads, with the reason, if:
  - the output has another shape,
  - its Ultralytics metadata says `dynamic=True`,
  - its metadata says `end2end=False` and does not say `nms=True`, or
  - its metadata `imgsz` is different from its input size.
- A failed load keeps the model that was active.
- After a model change, the app is stopped, on success and on failure.

## Capture

- The user picks the display. The app captures the Model input region of that display.
- When the display changes, the Run pauses and the app prepares a new Pipeline. On success, the app changes to it. On failure, the app keeps the old one. Then a Continuous run resumes.

| Setting        | Range      | Default |
| -------------- | ---------- | ------- |
| FPS limit      | 1 to 1000  | 240     |
| New frame wait | 1 to 20 ms | 2 ms    |

- **FPS limit** sets the shortest time between the starts of two captures that got a New frame in a Continuous run. A late frame is not followed by a burst of frames. It does not apply to Run once.
- A capture attempt that gets no new desktop update is not limited by the FPS limit: the app handles the pending commands and tries again.
- **New frame wait** is the longest wait for one capture attempt. It is independent of the FPS limit. It is also the longest time before a command is handled while the app waits for a New frame.
- A capture attempt that gets no new desktop update does no inference and sends no Movement. The app never processes an old image again.

## Target and output

| Setting            | Range                | Default        |
| ------------------ | -------------------- | -------------- |
| Confidence         | 0 to 100 %           | 50 %           |
| Target Y offset    | 0 to 100 %           | 50 %           |
| Sensitivity        | 1 to 10000 %         | 100 %          |
| Max delta          | 0 to 32767           | 512            |
| Move method        | Standard, Raw        | Standard       |
| Move interpolation | Auto, 0 to 100 %     | Auto           |

- **Confidence** is the Confidence threshold.
- The Selected target is the Candidate detection nearest to the center of the Model input region.
- **Target Y offset** sets the Aim point: 0 % is the top edge of the box, 50 % is its center, and 100 % is its bottom edge. It applies before the Sensitivity.
- **Sensitivity** is a whole percent. The gain is the Sensitivity divided by 100.
- **Max delta** limits the length of the Movement of one frame, after the Sensitivity. The direction stays the same. 0 means no limit.
- **Move method** is Standard move (the `MOVE` command) or Raw move (the `RAW_MOVE` command: MAKCU takes one USB report per move and reports a result later). A change applies to the next move, and it is saved.
- **Move interpolation** is Auto or a fixed percent. It applies only to a Standard move, so the UI disables it while Raw move is selected. A change is sent to MAKCU at once and again after each connect. If MAKCU does not confirm it, the error line shows the reason, and the Run still starts.
- The Rounding remainder goes to the next frame of the same Target track. A Target switch, a lost target, and the start of a Run clear it.
- The output is mouse movement only, through MAKCU. There is no controller output.
- MAKCU needs firmware V4.085 or newer. An older device is refused, and the Devices panel shows the error, such as `MAKCU firmware V4.084 is older than V4.085`.
- The UI shows the MAKCU connection, the firmware, and the Mouse route. The user can refresh the routes, rescan for the device, and connect again.

## Runs

- A Continuous run is active while one or more Hold shortcuts are down.
- Run once starts from its Shortcut or from the `Run once` button.
- The app handles a stop, a shutdown, a model change, and a display change at two points: before a capture, and after the inference and before the output. A capture and an inference cannot be interrupted, so the time to stop includes the stage that is in progress.
- While the app waits for the FPS limit, it handles a command immediately.
- While the FPS limit holds the app back, the engine keeps one CPU core busy for about 1 ms before each capture. The rest of the wait sleeps.
- **Low-latency mode** (default off) lowers the worst latency. It keeps one CPU core fully busy during a Run. A change of this setting loads the model again.

## Hotkeys

| Action          | Default Shortcut |
| --------------- | ---------------- |
| Hold continuous | `F8`             |
| Run once        | `F9`             |

- An action can have more than one Shortcut.
- One Shortcut cannot belong to two actions.
- A keyboard Shortcut can use Ctrl, Shift, and Alt. The Windows key is not supported.
- An XInput Shortcut can be a button, LT, or RT. Sticks are not supported.
- Shortcuts work when the app does not have the focus.
- While the user records a new Shortcut, no Shortcut starts an action. Esc cancels the recording.

## Diagnostics

- **Log level**: `off`, `error`, `warn`, `info` (default), `debug`, `trace`. Each session writes `logs/session-<unix_ms>-<pid>.log`.
- **GPU debug** (default off) applies only at the start of the app. It makes inference slower.
- **Frame trace** (default off): each Run writes `logs/trace-<unix_ms>.jsonl`. The file has a header line, one `frame` line for each processed frame, and a footer line. A change of this setting applies to the next Run.
  - The trace never slows a Run. A record that cannot be written in time is dropped, and the footer has the count of dropped records.
  - A trace file that cannot be opened does not stop the Run.
- The UI shows the Loop metrics: the frame period, the RMS error, the overshoots, and the time to reach the target.
- The UI shows the Raw move counters: moves sent, rejected, and timed out, and the last reject reason. The counters start again at each MAKCU connection. After 8 rejects in a row, the UI warns to select Standard move or to lower Max delta.
- The UI shows timing statistics for each stage of a frame. The Cold run is shown apart and is not part of the statistics.

## Settings file

- `settings.json` has no version and no migration.
- A field that is missing gets its default. The other fields keep their values.
- An unknown field is removed. A value that is out of range or of the wrong type is replaced with the default of its field. An invalid hotkey entry is removed. The other fields keep their values.
- Invalid JSON, or a file that cannot be read, makes the app use the defaults for the whole file.
- The app shows one warning that names the removed fields and the reset fields.
- Loading does not change the file. The next save writes the settings that the app uses.
- When the file could not be read or parsed, the next save first copies it to `settings.json.bak`. An earlier `settings.json.bak` is replaced.
- Hotkeys, the selected display, and the selected model are part of the settings.
