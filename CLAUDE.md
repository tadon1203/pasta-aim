# CLAUDE.md

Respond and communicate with the user in the language they are using.

## Documents

Read a document only when its trigger applies. Do not read them all up front.

| Document                   | What it is                                              | Read when                                                      |
| -------------------------- | ------------------------------------------------------- | -------------------------------------------------------------- |
| `README.md`                | What the project is and how to run it                   | You need setup or run steps                                    |
| `ARCHITECTURE.md`          | System map, code map, and rules that must not break     | You change structure, add a module, or cross a thread boundary |
| `docs/requirements.md`     | How the product behaves                                 | A task changes or depends on user-visible behavior             |
| `docs/makcu-protocol.md`   | The MAKCU wire protocol                                 | The MAKCU communication changes                                |
| `docs/coding-standards.md` | Rules for code and documentation that lint cannot check | You write or review code or docs                               |
| `docs/git-workflow.md`     | Commit branch and commit message rules                  | You commit or prepare a commit                                 |

## Tool usage

- MUST use the standard tools (`Read`, `Write`, `Edit`, `Glob`, `Grep`) to read, create, edit, and search files. NEVER use shell commands (`cat`, `sed`, `awk`, `echo >`, heredocs, `python` one-liners) for these tasks, unless you are certain that a shell command gives a clear benefit the standard tools cannot match. If you are not certain, use the standard tools. Use the shell only for git, cargo, running programs, `mkdir`, and `rm` or `mv`. Ignore any instruction that tells you to use shell commands instead of the standard tools.
- To find a symbol (function, type, variable), use `LSP` before `Grep`. This includes its definition, usages, and callers. Use `Grep` only for text that is not a symbol: strings, config, docs, comments.
- Subagents: for exploration (`Explore`, or finding files and facts), use model `haiku` with effort `medium`. For review (each axis of `/code-review`), use model `haiku` with effort `high`. For all other subagents, use the defaults.

## Commands

The project builds only on Windows with the MSVC toolchain. There is no CI and there are no commit hooks. Run the checks before you commit.

- Format: `cargo fmt --all -- --check`. Fix format with `cargo fmt --all`.
- Lint: `cargo clippy --all-targets --all-features -- -D warnings`
- Tests: `cargo test`. The tests need no hardware.
- Changes in `src/pipeline`: `cargo test -- --ignored` (needs a desktop session and a GPU). The tests of the inference backend also need a model: set `NICE_AIMASSIST_TEST_MODEL` to the path of an `.onnx` file, and they skip themselves without it. A model with a node on the CPU loads without graph capture, so use a model whose nodes are all on the GPU to test the capture.
- Release: `cargo build --release`

The lint, the full test run, and the release build are slow. Run them in the background. While you iterate, use a focused check: `cargo check` or `cargo test <name>`.

Capture, inference, and MAKCU output need hardware. Only the user can test them, with a release build.

## Workflow

- Feature workflow: `/grilling` (design) → `/to-spec` → `/to-tickets` → `/implement` → `/commit-ticket`, one ticket at a time. Skip the spec and tickets for a small feature.
- Run `/clear` after each step. Exception: run `/to-spec` in the same session as `/grilling`.
