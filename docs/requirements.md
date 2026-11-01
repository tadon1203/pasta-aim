# Requirements

A Windows app that captures the center of a display, finds targets with a YOLO model, and moves the mouse toward the target through a MAKCU device. A detection is one box that the model reports, with a confidence and a class. A target is the accepted detection nearest to the center of the capture region. Low and stable latency comes before features.

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

- The user picks the display. The app captures the capture region of that display. The capture region is the rectangle at the center of the display that has the size of the model input.
- When the display changes, the run pauses and the app prepares a new pipeline. A pipeline is the capture, the GPU preprocess, the inference session, and the model for one display, and it is built and replaced as a unit. On success, the app changes to the new pipeline. On failure, the app keeps the old one. Then a continuous run resumes.

| Setting        | Range      | Default |
| -------------- | ---------- | ------- |
| FPS limit      | 1 to 1000  | 240     |
| New frame wait | 1 to 20 ms | 2 ms    |

- A frame is one image of the capture region, with the time data of its capture. A new frame is a frame that comes from a new desktop update.
- **FPS limit** sets the shortest time between the starts of two captures that got a new frame in a continuous run. A late frame is not followed by a burst of frames. It does not apply to run once.
- A capture attempt that gets no new desktop update is not limited by the FPS limit: the app handles the pending commands and tries again.
- **New frame wait** is the longest wait for one capture attempt. It is independent of the FPS limit. It is also the longest time before a command is handled while the app waits for a new frame.
- A capture attempt that gets no new desktop update does no inference and sends no move. The app never processes an old image again.

## Target and output

| Setting            | Range                | Default        |
| ------------------ | -------------------- | -------------- |
| Confidence         | 0 to 100 %           | 50 %           |
| Target Y offset    | 0 to 100 %           | 50 %           |
| Sensitivity        | 1 to 10000 %         | 100 %          |
| Max delta          | 0 to 32767           | 512            |
| Move interpolation | Auto, 0 to 100 %     | Auto           |

- **Confidence** is the confidence threshold: the lowest confidence at which a detection is accepted.
- **Target Y offset** sets the aim point, which is the point inside the box of the target that the output moves toward: 0 % is the top edge of the box, 50 % is its center, and 100 % is its bottom edge. It applies before the sensitivity.
- **Sensitivity** is a whole percent. The gain is the sensitivity divided by 100.
- **Max delta** limits the length of the move of one frame, after the sensitivity. A move is the mouse movement that the engine sends for one frame, in whole counts. The direction stays the same. 0 means no limit.
- **Move interpolation** is Auto or a fixed percent. A change is sent to MAKCU at once and again after each connect. If MAKCU does not confirm it, the error line shows the reason, and the run still starts.
- A track is the target followed across consecutive frames: two boxes that overlap enough are the same target. A target switch is the first frame of a track.
- The remainder is the part of a move that is lost when the move is rounded to whole counts. The remainder goes to the next frame of the same track. A target switch, a lost target, and the start of a run clear it.
- The output is mouse movement only, through MAKCU. There is no controller output.
- MAKCU needs firmware V4.076 or newer. An older device is refused, and the Devices panel shows the error, such as `MAKCU firmware V4.075 is older than V4.076`.
- The UI shows the MAKCU connection, the firmware, and the Mouse route. The user can refresh the routes, rescan for the device, and connect again.
- The MAKCU port is Auto or a port that the user chooses from the list of serial ports. Auto tries the ports of the MAKCU chips. A chosen port is the only one that is tried, also after a disconnect, even when its chip is not a MAKCU chip. A change connects again at once and is saved. If the chosen port is not present, the Devices panel shows the connect error and `not found`, and the choice is kept.

## Runs

- A run is one period in which the engine processes frames, from a start to a stop. A continuous run is a run that processes frames until it is stopped. Run once is a run that processes one new frame and then stops.
- A continuous run is active while one or more hold shortcuts are down. A shortcut is a keyboard key combination or an XInput control that the user binds to an action. A hold shortcut keeps a continuous run active while it is held down.
- Run once starts from its shortcut or from the `Run once` button.
- The app handles a stop, a shutdown, a model change, and a display change at two points: before a capture, and after the inference and before the output. A capture and an inference cannot be interrupted, so the time to stop includes the stage that is in progress.
- While the app waits for the FPS limit, it handles a command immediately.
- While the FPS limit holds the app back, the engine keeps one CPU core busy for about 1 ms before each capture. The rest of the wait sleeps.
- **Low-latency mode** (default off) lowers the worst latency. It keeps one CPU core fully busy during a run. A change of this setting loads the model again.

## Hotkeys

| Action          | Default shortcut |
| --------------- | ---------------- |
| Hold continuous | `F8`             |
| Run once        | `F9`             |

- An action can have more than one shortcut.
- One shortcut cannot belong to two actions.
- A keyboard shortcut can use Ctrl, Shift, and Alt. The Windows key is not supported.
- An XInput shortcut can be a button, LT, or RT. Sticks are not supported.
- Shortcuts work when the app does not have the focus.
- Shortcuts are read by polling. They do not take the key from other apps, so a shortcut key also reaches the focused app and the game.
- Recording a new shortcut reads both the keyboard and the controller. Esc cancels the recording.
- While the user records a new shortcut, no shortcut starts an action.

## Diagnostics

- **Log level** is the lowest severity that the session log writes: `off`, `error`, `warn`, `info` (default), `debug`, or `trace`. A session log is the text file of one start of the app, and each session writes `logs/session-<unix_ms>-<pid>.log`.
- **GPU debug** (default off) applies only at the start of the app. It makes inference slower. With GPU debug on, a GPU fault writes one log line with the pipeline, the stage, the error, the HRESULT, the D3D11 and D3D12 removed reasons, and the D3D12 debug layer messages.
- **CPU fallback nodes** is the number of model nodes that ONNX Runtime assigned to the CPU. The diagnostics panel and its copied text show it as `CPU fallback nodes: N`, or `unknown` when ONNX Runtime does not report the assignment.
- **Frame trace** (default off): each run writes `logs/trace-<unix_ms>.jsonl`. A change of this setting applies to the next run.
  - The file has a `header` line, a `frame` line for each processed frame, an `event` line for each trace event, and a `footer` line. The file has no version: the header names the version of the app.
  - The header has what a reader needs to read the file alone: the app version and build, the clock (the QPC frequency, and the QPC value and the wall clock at the start of the run), the run, MAKCU (port, firmware, baud, and the move interpolation), the model, and the settings of the run.
  - Every time in a line is in microseconds from the start of the run. A `frame` line has the time of each stage, `total_us` (from the present time, which is the time of the desktop update that made the frame, to the end of the output), the detections with the highest confidence first (at most 8, and `candidate_total` has the full count), the target, the track, the aim point error, the move before rounding, and the move that was sent.
  - A trace event is something that happened in a run and is not a frame. The trace events are: MAKCU disconnected, MAKCU connected (port, firmware, baud), the move interpolation failed, the pipeline changed (the next frame is the first frame), a frame result dropped (with the reason), and messages that the telemetry queue refused.
  - The trace never slows a run. A message that cannot be taken in time is dropped and counted. A dropped frame record is then missing from the trace, the statistics, and the loop metrics. A frame record is the data of one processed frame: its times, its detections, its target, and its move. The footer has the count of frames and the count of dropped messages.
  - A trace file that cannot be opened or written does not stop the run.
- The UI shows the loop metrics, which are measurements of how well the output follows the target: the frame period, the RMS error, the overshoots, and the time to reach the target.
- The UI updates the statistics and the last target at most 10 times each second. A run that stops updates them at once.
- The UI shows timing statistics for each stage of a frame. The first frame after a model load or a pipeline change is shown apart and is not part of the statistics.

## Settings file

- `settings.json` has no version and no migration. The field names can change between versions; a file from an older version loses the renamed fields once, and they take their defaults.
- A field that is missing gets its default. The other fields keep their values.
- An unknown field is ignored, and the next save drops it.
- A numeric value that is out of range is clamped to its range. A number that is negative or above 65535 is a wrong type instead (see the next rule).
- An invalid hotkey entry is removed, with a warning. The other fields keep their values.
- A value of the wrong type, invalid JSON, or a file that cannot be read makes the app use the defaults for the whole file, with one warning.
- Loading does not change the file. The next save writes the settings that the app uses. When the file could not be read or parsed, the next save first copies it to `settings.json.bak`, and an earlier `settings.json.bak` is replaced.
- Hotkeys, the selected display, and the selected model are part of the settings.
