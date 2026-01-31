# Configuration, Paths, and Maintenance Conventions

This page collects "operational" conventions that are easy to forget but frequently needed when
debugging or evolving the repo:

- where persistent state lives (`.codexpotter/`, config files, history)
- how model config is resolved for display
- sandbox / approval behavior when driving the upstream `codex app-server`
- how to run tests and manage snapshots

## Paths & persistence (`.codexpotter/`)

### Per-project (under the current working directory)

Created by `cli/src/project.rs`:

- `.codexpotter/projects/YYYYMMDD_N/MAIN.md`
  - the progress file that the agent is instructed to read/update each round
  - the file contains front matter fields (`status`, `finite_incantatem`, `short_title`) plus task lists
- a gitignored knowledge base directory
  - a scratchpad for intermediate findings; intentionally not committed

### Per-user (under the home directory)

- `~/.config/codexpotter/config.toml` (or `$XDG_CONFIG_HOME/codexpotter/config.toml`)
  - currently used for `notice.hide_gitignore_prompt` (`cli/src/config.rs`)
- `~/.codexpotter/history.jsonl`
  - prompt history for the bottom composer (see `tui-chat-composer.md`)
- `~/.codexpotter/codex-compat/`
  - a "Codex home" shim created by `cli/src/codex_compat.rs`
  - contains symlinks to `~/.codex/config.toml` and `~/.codex/auth.json`

Everything under `.codexpotter/` is intended to be ignored by git.

## Model config resolution (for display)

The TUI reads a subset of upstream Codex's config layering to determine the model label shown in
the startup banner / status UI.

Entry point: `tui/src/codex_config.rs` (`resolve_codex_model_config`).

### CODEX_HOME

- If `$CODEX_HOME` is set and non-empty, it is used and `canonicalize()`d (errors if invalid).
- Otherwise it defaults to `~/.codex` (directory existence is not validated).

### Layering order (subset)

This crate intentionally implements only the parts needed for model display:

1. system: `/etc/codex/config.toml` (Unix only)
2. user: `$CODEX_HOME/config.toml`
3. project layers: `.codex/config.toml` from "project root" to `cwd`

Project root is discovered by walking parents until a configured marker matches
(`project_root_markers`, default: `[".git"]`).

### Profile selection

If `profile = "..."` is set, model and reasoning effort are resolved from `profiles.<name>.*`
first, then fall back to the top-level `model` / `model_reasoning_effort`.

## Sandbox and approvals (app-server bridge)

`codex-potter` is non-interactive, so it must avoid states where the app-server is waiting for user
approvals.

### CLI flags

- `--sandbox <default|read-only|workspace-write|danger-full-access>`
  - controls both:
    - the process spawn sandbox flag passed to `codex app-server` (when not `default`)
    - the thread sandbox passed to `thread/start`
- `--yolo` / `--dangerously-bypass-approvals-and-sandbox`
  - passes upstream Codex's bypass flag when spawning the app-server
  - requests `danger-full-access` at the thread level

Implementation: `cli/src/app_server_backend.rs` (`AppServerLaunchConfig::from_cli`).

### Approval policy

- `thread/start` requests `approvalPolicy: "never"` (`cli/src/app_server_backend.rs`).
- `thread/start` requests `archived: true` (`cli/src/app_server_backend.rs`).
- If the app-server still emits approval requests, `codex-potter` auto-accepts them to avoid
  hanging (see `handle_server_request` in `cli/src/app_server_backend.rs`).

## Tests and snapshot maintenance

### Formatting and linting

- `cargo fmt`
- `cargo clippy`

### Running tests

- workspace: `cargo test`
- TUI crate only: `cargo test -p codex-tui`

### Snapshot tests (`insta`)

The TUI relies on snapshot tests for rendered output.

Workflow:

- run tests to generate new snapshots: `cargo test -p codex-tui`
- list pending snapshots: `cargo insta pending-snapshots -p codex-tui`
- inspect a specific file: `cargo insta show -p codex-tui path/to/file.snap.new`
- accept all pending snapshots for the crate (only if intended): `cargo insta accept -p codex-tui`

If you don't have the tool:

- `cargo install cargo-insta`
