# AGENTS.md

Talk to the user in the language they use.

## Writing

- Write all text in ASD-STE100 Simplified Technical English. This covers comments, docs, commit messages.
- Use active voice and simple present tense. Use one word for one meaning.
- Use plain, common words. Avoid rare words, slang, and metaphors. Pick a word that states the meaning directly.

## Project

### Product

The app captures the center of a display. The capture size equals the model input size. A model finds targets in the capture. The app moves the mouse toward the targets. Latency is more important than features.

### Documents

Read a document only when its trigger applies.

| Document                 | Content                    | Read when                       |
| ------------------------ | -------------------------- | ------------------------------- |
| `README.md`              | Project summary, run steps | You need setup or run steps     |
| `docs/makcu-protocol.md` | MAKCU wire protocol        | The MAKCU communication changes |

## Code

### Structure

Each top-level folder in `NiceAimassist/` is a feature, with two exceptions: `Shared/` and the two layers above the features.

```text
App → Loop → Capture, Detect, Aim, Dataset, Hotkey, Mouse → Shared
Settings → the settings type of each feature
Detect, Dataset → Capture
```

- A feature does not use the internals of another feature. A dependency points to the right in the diagram.
- `Loop` owns the order of one frame: acquire, detect, aim, move, record. A feature does not call another feature to start a side effect. Example: `Detector` does not call `DatasetRecorder`.
- `Shared/` (`Gpu`, `Logging`) does not depend on a feature.
- `Settings/` and `App/` are layers above the features. They combine the features.
- Put a subfolder in one feature only when the folder has many files with different roles. Use at most two levels. Example: `Detect/Runtime`, `Mouse/Makcu`.
- Keep the namespace equal to the folder path.
- Make a type in a subfolder `internal` when no other feature uses it.
- A feature can own an interface that another feature implements. Example: `Capture/IFrameSource`, which `DesktopCapture` implements and `Loop` and `Dataset` use.
- A setting that changes while the app runs flows one way: the settings window or the file changes `SettingsFile.Current`, and `SettingsFile.Changed` tells the listeners. Do not write the file to pass a change inside the app.
- Put a test in the folder that matches the folder of the type under test.

### Rules

- Errors: throw an exception whose message names the failing step or value. Define an exception type only when a caller catches it by kind.
- Use unsafe code only for calls to operating system, GPU, and machine learning runtime APIs.
- Release native handles in the order that the owning type states.

### Comments

- A comment states only a non-obvious constraint that the code does not show and a change could break. Examples: an invariant, an ordering or drop rule, a safety condition, a platform quirk.
- Never describe what the code does, what a name means, a change, or its history.

### Tests

- Tests need no hardware. Mark a test that needs hardware as skipped, with a reason.
- Fake only the MAKCU port. Keep logic separate from GPU and OS calls, so the logic can be tested directly.
- Keep a test only if it protects a requirement or an invariant.

## Workflow

### Commands

The project builds only on Windows with the .NET 10 SDK. There is no CI and there are no commit hooks. Run the checks before you commit.

- Build and lint: `dotnet build NiceAimassist.sln -c Release`. Warnings fail the build.
- Check format: `dotnet format NiceAimassist.sln --verify-no-changes`. Fix format: `dotnet format NiceAimassist.sln`.
- Tests: `dotnet test NiceAimassist.sln`.
- Focused test: `dotnet test NiceAimassist.sln --filter "FullyQualifiedName~<ClassName>"`. Filter on a class name.
- Release: `dotnet publish NiceAimassist/NiceAimassist.csproj -c Release`.

The build, the full test run, and the publish are slow. Run them in the background. While you iterate, use a focused test.

Capture, inference, and MAKCU output need hardware. Only the user can test them, with a release build.

### Git

- Commit to `main`. Use a branch for a large or risky change.
- Message: `<type>(<scope>): <description>` (Conventional Commits). Use the imperative, lowercase, no period. Mark a breaking change with `!`.
