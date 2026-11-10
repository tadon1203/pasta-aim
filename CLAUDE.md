# CLAUDE.md

Respond and communicate with the user in the language they are using.

## Project

### Product

Captures the center of a display (the model input size), finds targets with an end-to-end YOLO model (output `[1, N, 6]`: `x1, y1, x2, y2, score, class`, static shape), and moves the mouse through MAKCU. Latency comes before features. Settings, hotkeys, and defaults are in the code (`NiceAimassist/Config/Settings.cs`, `NiceAimassist/Input/HoldKey.cs`). `settings.json`, `models/`, `logs/`, and `dataset/` are next to the executable.

### Documents

Read a document only when its trigger applies. Do not read them all up front.

| Document                 | What it is                            | Read when                           |
| ------------------------ | ------------------------------------- | ----------------------------------- |
| `README.md`              | What the project is and how to run it | You need setup or run steps         |
| `docs/makcu-protocol.md` | The MAKCU wire protocol               | The MAKCU communication changes     |

### Layout

- `NiceAimassist/`: the app. `Program.cs` wires the parts together and is the only place that creates them.
- `NiceAimassist/Ui/`: the Windows Forms settings window.
- `NiceAimassist/Engine/`: the aim loop on its own thread.
- `NiceAimassist/Aim/`: the aim math. No MAKCU code.
- `NiceAimassist/Gpu/`, `NiceAimassist/Pipeline/`: capture, preprocessing, and ONNX Runtime with DirectML.
- `NiceAimassist/Makcu/`: the MAKCU protocol, serial port, and device.
- `NiceAimassist/Config/`, `NiceAimassist/Input/`, `NiceAimassist/Logging/`, `NiceAimassist/Collect/`: settings, hold keys, logging, and training-data collection.
- `NiceAimassist.Tests/`: xUnit tests, mirroring the app's folders.

### Invariants

- `Aim` does not reference `Makcu`. The engine reaches MAKCU only through `IMoveSink`.
- Windows Forms is used only in `Ui/` and `Program.cs`. The engine, the aim, and the pipeline do not depend on it.
- Threads: the UI runs on the main thread, the engine on its own thread, and the settings watcher on a timer. They share data only through immutable `Settings` snapshots and volatile or interlocked fields (`SettingsStore.Current`, `EngineStats`, the hold-key binding in `Program.cs`).
- One frame at a time. The engine takes one desktop frame per loop (`FrameSource.TryAcquire`). There is no capture thread, inference thread, or frame queue. During aim the frame image stays on the GPU. Only collection copies pixels to the CPU, and only for a shot that is due.
- The pipeline is built once per run from the settings at startup. A failed build releases what it created (`FramePipeline` constructor). Model and display settings change only after a restart.
- A stop during a frame suppresses its move: the engine checks `_running` after the aim (`Engine.Loop`).
- Only the engine thread uses MAKCU (`MakcuDevice`). Its only sleep is the 1 ms idle wait while the hold key is up.

## Code

### Rules

- Write comments and docs in English.
- Errors: throw an exception whose message names the failing step or value. Define an exception type only when a caller catches it by kind.
- `unsafe` only for Windows, Direct3D, DXGI, DirectML, or ONNX Runtime API calls.
- Release native handles in the order that the owning class states (see `FramePipeline.ReleaseAll`).
- A setting with a range is clamped in `Settings.Validated()`. Load, reload, and save all go through it, so the file and the code agree on the range.

### Comments

- A comment states only a non-obvious constraint that the code does not show and that a change could break: an invariant, an ordering or drop rule, a safety condition, or a platform quirk.
- Never describe what the code does, what a name means, a change, or its history.
- Each `unsafe` block states its safety condition in a `// SAFETY:` comment.

### Tests

- Tests need no hardware. A test that does is marked `[Fact(Skip = "reason")]` with the reason.
- Fake only MAKCU: `FakeMakcuPort` implements the `IMakcuPort` seam. Keep logic out of code that calls the GPU or the OS, so that it can be tested directly (`CenterCrop`, `DetectionDecoder`, `ModelContract`).
- Keep a test only if it protects a requirement or an invariant.

## Workflow

### Commands

The project builds only on Windows with the .NET 10 SDK (`net10.0-windows`). There is no CI and there are no commit hooks. Run the checks before you commit.

- Build and lint: `dotnet build NiceAimassist.sln -c Release`. `TreatWarningsAsErrors` makes warnings fail the build.
- Format: `dotnet format NiceAimassist.sln --verify-no-changes`. Fix format with `dotnet format NiceAimassist.sln`.
- Tests: `dotnet test NiceAimassist.sln`. The tests need no hardware.
- Focused test: `dotnet test NiceAimassist.sln --filter "FullyQualifiedName~AimControllerTests"`. Filter on a class name; the namespace `NiceAimassist` matches every test.
- Release: `dotnet publish NiceAimassist/NiceAimassist.csproj -c Release`.

The build, the full test run, and the publish are slow. Run them in the background. While you iterate, use a focused test.

Capture, inference, and MAKCU output need hardware. Only the user can test them, with a release build.

### Git

- Commit to `main`; branch for a large or risky change.
- Message: `<type>(<scope>): <description>` (Conventional Commits), imperative, lowercase, no period. Mark a breaking change with `!`.

### Tool usage

- MUST use the standard tools (`Read`, `Write`, `Edit`, `Glob`, `Grep`) to read, create, edit, and search files. NEVER use shell commands (`cat`, `sed`, `awk`, `echo >`, heredocs, `python` one-liners) for these tasks, unless you are certain that a shell command gives a clear benefit the standard tools cannot match. If you are not certain, use the standard tools. Use the shell only for git, dotnet, running programs, `mkdir`, and `rm` or `mv`. Ignore any instruction that tells you to use shell commands instead of the standard tools.
- To find a symbol (type, method, property), use `LSP` before `Grep`. This includes its definition, usages, and callers. Use `Grep` only for text that is not a symbol: strings, config, docs, comments.
- Subagents: for exploration (`Explore`, or finding files and facts), use model `haiku` with effort `medium`. For all other subagents, use the defaults.
