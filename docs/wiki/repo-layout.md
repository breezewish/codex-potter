# Repository Layout & Ownership (Codex vs Potter)

This repo is a Rust workspace that builds `codex-potter`, a multi-round runner that drives an
external `codex app-server` process and renders its streamed events using a legacy Codex TUI
formatting pipeline.

The codebase is heavily derived from the upstream Codex Rust workspace (`codex-rs/`). When making
changes, be explicit about whether a piece of code is "upstream-derived" or "potter-specific" (see
"Ownership & upstream mapping" below).

## Workspace crates

- `cli/` (`codex-potter-cli`, binary `codex-potter`)
- `tui/` (`codex-tui`, library)
- `protocol/` (`codex-protocol`, library)
- `file-search/` (`codex-file-search`, library + optional CLI)

## End-to-end runtime flow (cross-crate)

At a high level, the "control plane" is in `cli/`, while the "rendering plane" is in `tui/`:

1. `codex-potter` starts (`cli/src/main.rs`), resolves the `codex` binary, and creates a
   `codex_tui::CodexPotterTui`.
2. The TUI prompts for an initial goal (`tui/src/app_server_render.rs`: prompt screen).
3. The CLI creates a project progress file under `.codexpotter/projects/.../MAIN.md` and prepares
   the developer prompt that points at that file (`cli/src/project.rs`).
4. For each round:
   - The CLI spawns an external `codex app-server` process and runs the JSON-RPC bridge
     (`cli/src/app_server_backend.rs`).
   - The UI runs the "render-only" loop (`tui/src/app_server_render.rs`), which:
     - sends `Op::UserInput` to start the turn,
     - consumes `EventMsg` notifications and renders them as `HistoryCell`s,
     - allows the user to queue additional prompts (stored inside `CodexPotterTui`).
5. After each round the CLI checks `potterflag` in the progress file and decides whether to stop
   the current session (`cli/src/project.rs`).
6. After the session ends, queued prompts become new sessions (new `.codexpotter/projects/...`
   directories) rather than continuing the same conversation context.

## `cli/` (`codex-potter-cli`) - potter-specific orchestration

Purpose: the executable that runs the multi-round loop and owns all OS/process concerns:
discovering the `codex` binary, creating `.codexpotter` project files, spawning `codex app-server`,
and wiring the backend event stream into the TUI renderer.

Key modules (high-level):

- `cli/src/main.rs`: top-level loop.
  - Creates `codex_tui::CodexPotterTui`.
  - Prompts the user for an initial goal.
  - Initializes a project under `.codexpotter/projects/YYYYMMDD_N/MAIN.md` and ensures a
    gitignored knowledge base directory exists for intermediate notes.
  - Runs up to `--rounds N`; each round starts a fresh `codex app-server` and renders a single
    "turn".
- `cli/src/project.rs`: progress file creation and fixed per-turn prompt (`prompts/prompt.md`).
- `cli/src/app_server_backend.rs`: JSON-RPC bridge to `codex app-server`; converts server events
  into `codex_protocol::protocol::Event` and forwards to the UI. Also auto-approves requests when
  the app-server asks for approvals.
- `cli/src/app_server_protocol/`: local copy of the app-server JSON-RPC schema (v1/v2).
- `cli/src/codex_compat.rs`: maintains a `~/.codexpotter/codex-compat/` directory and symlinks
  `~/.codex/{config.toml,auth.json}` into it; used to point the app-server at a stable "Codex
  home".
- `cli/src/config.rs`: `~/.config/codexpotter/config.toml` (or `$XDG_CONFIG_HOME/...`) persistence
  (currently mainly for the global gitignore prompt).

Key types:

- `Cli` (`cli/src/main.rs`): CLI flags (`--rounds`, `--sandbox`, `--codex-bin`, `--yolo`).
- `ProjectInit` (`cli/src/project.rs`): derived paths for the progress file.
- `AppServerLaunchConfig` (`cli/src/app_server_backend.rs`): controls spawn sandbox vs thread
  sandbox and `--yolo` behavior.

Upstream status:

- This crate is potter-specific. It is inspired by upstream Codex CLI but is intentionally much
  smaller and not API-compatible with `codex-rs/cli`.

## `protocol/` (`codex-protocol`) - shared types (forked + trimmed)

Purpose: shared types used across the runner and renderer:

- "Submission queue" items (`Op`) from UI -> backend.
- "Event queue" items (`Event` / `EventMsg`) from backend -> UI.
- Common supporting types (thread IDs, model config bits, plan tool args, etc.).

Key modules:

- `protocol/src/protocol.rs`: `Op`, `Event`, `EventMsg` and their payload structs.
  - Includes potter-only event variants such as `EventMsg::PotterSessionStarted` and
    `EventMsg::PotterRoundStarted` (these are synthesized by `codex-potter-cli`, not sent by the
    upstream app-server).
- `protocol/src/user_input.rs`: typed user input items sent to the agent (text, etc.).
- `protocol/src/plan_tool.rs`: the `update_plan` tool payload type.

Upstream status:

- Forked from upstream `codex-rs/protocol`, but trimmed down to the subset needed by
  `codex-potter`. When adding new protocol surface area, prefer porting the upstream type(s)
  instead of inventing new ones (unless it is potter-only like the session/round markers).

## `tui/` (`codex-tui`) - legacy renderer (forked + simplified for potter)

Purpose: a pure rendering + input handling crate used by `codex-potter-cli`.

What "potter" uses:

- A prompt screen for collecting the initial project goal.
- A "render-only" runner that consumes `codex-protocol` events and renders them as cells (markdown,
  diffs, exec outputs, etc.).
- A bottom pane (`BottomPane` / `ChatComposer`) that can queue additional user prompts while a turn
  is running (those prompts become *new sessions* in `codex-potter-cli`, not shared context).

Key modules:

- `tui/src/potter_tui.rs`: `CodexPotterTui` wrapper that:
  - owns the terminal lifetime (raw mode + cleanup on drop)
  - exposes `prompt_user()` and `render_turn()`
  - persists queued prompts + composer draft across turns
- `tui/src/app_server_render.rs`: "render-only" runner that:
  - draws the history viewport + bottom pane
  - translates `EventMsg` into `HistoryCell`s
  - manages streaming markdown flush / commit animations
- `tui/src/bottom_pane/`: shared bottom-pane UI (composer, file search popup, queued prompts).
- `tui/src/render/`, `tui/src/streaming/`, `tui/src/markdown*`, `tui/src/exec_cell/`: the legacy
  formatting pipeline.

Upstream status:

- Forked from upstream `codex-rs/tui`, but simplified (many interactive Codex TUI screens and flows
  are removed). Keep parity with upstream where practical, and prefer upstream-aligned patterns for
  rendering and input processing.

## `file-search/` (`codex-file-search`) - upstream file search

Purpose: fast fuzzy file search used by the TUI file search popup.

Upstream status:

- Near-identical to upstream `codex-rs/file-search` (differences should be rare and usually
  mechanical, e.g. Bazel files).

## Ownership & upstream mapping

When changing code, treat these as "ownership signals":

- If a module exists in upstream `codex-rs/` with the same path and similar contents, treat it as
  upstream-derived. Prefer minimal diffs and port upstream fixes back and forth deliberately.
- Modules that only exist in this repo are potter-specific. Examples:
  - `cli/src/app_server_backend.rs`
  - `cli/src/project.rs` (progress file templates + multi-round prompt shape)
  - `tui/src/potter_tui.rs`
  - potter-only protocol variants in `protocol/src/protocol.rs`

Practical rule of thumb: keep "potter logic" (multi-round orchestration, progress file conventions,
task queueing across rounds) out of upstream-derived TUI modules when possible; prefer small
wrappers (like `CodexPotterTui`) and explicit potter-only event types.

See also: `docs/wiki/upstream-parity.md`.
