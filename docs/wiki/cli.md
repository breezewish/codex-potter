# codex-potter CLI

`codex-potter` is a TUI CLI that drives a **multi-round** Codex workflow using the **legacy**
`codex-tui` formatting pipeline (Markdown, TODO lists, diffs, exec output blocks, shimmer,
streaming, etc), powered by an external `codex app-server` process.

Unlike `codex exec`, this tool does **not** run codex-core directly — it launches an external
`codex app-server` process and renders the streamed events.

This is developer-facing documentation. Start at `README.md` for the full code wiki, and see
`core-architecture.md` for the end-to-end flow.

## Ownership

- Potter-specific: the `codex-potter` runner lives in `cli/` and owns the multi-round/session
  orchestration.
- Upstream-derived dependency: the spawned `codex app-server` process is part of the upstream Codex
  CLI, and most protocol/event semantics are defined by upstream.

## Workflow

1. Validates that a `codex` binary is available (via PATH, unless `--codex-bin` is provided).
2. Optionally recommends adding `.codexpotter/` to your global gitignore.
3. Prompts once for your project goal, then creates:
   - `.codexpotter/projects/YYYY/MM/DD/N/MAIN.md` (progress file)
   - a gitignored knowledge base directory (scratchpad for intermediate findings)
4. Runs up to N rounds (default 10). Each round:
   - starts a fresh `codex app-server` (one app-server thread + one `turn/start`)
   - injects a fixed developer prompt pointing at the progress file
   - submits a fixed prompt: `Continue working according to the WORKFLOW_INSTRUCTIONS`
5. Stops early for the current project if the progress file front matter contains `finite_incantatem: true`
   (checked after each round; queued projects continue normally).

## CLI interface

```sh
codex-potter [OPTIONS] [COMMAND]
```

Options:

- `--codex-bin <path>`: Path to the `codex` binary to launch in app-server mode.
  - Also configurable via `CODEX_BIN` (defaults to `codex`).
- `--rounds <n>`: Number of turns to run (default: 10; must be >= 1).
  - For `resume`, this controls how many additional rounds are run after replay.
- `--sandbox <mode>`: Sandbox mode to request from Codex per turn.
  - One of: `default` (default), `workspace-write`, `read-only`, `danger-full-access`.
  - `default` matches `codex`'s default behavior: no `--sandbox` flag is passed to the app-server
    and the thread sandbox is left unspecified.
- `--dangerously-bypass-approvals-and-sandbox`: Launch `codex app-server` in Codex's `--yolo` mode.
  - Alias: `--yolo`.

Examples:

```sh
codex-potter
codex-potter --codex-bin ./target/debug/codex
codex-potter --rounds 5
codex-potter --sandbox workspace-write
codex-potter --yolo
codex-potter resume 2026/02/01/1
codex-potter resume 2026/02/01/1 --yolo
codex-potter --yolo resume .codexpotter/projects/2026/02/01/1
```

## Commands

### `resume <PROJECT_PATH>`

Replays a previous CodexPotter project (history-only) and then prompts for a follow-up action.

At the moment the action picker has a single action:

- `Iterate N more rounds`: continue running additional rounds on the same project directory.
  - `N` is controlled by `--rounds` (default: 10).

`PROJECT_PATH` is resolved to a unique progress file (`.../MAIN.md`) using a small candidate set:

- If `<PROJECT_PATH>` is an absolute path:
  - If it is a `MAIN.md` file, it is used as-is.
  - Otherwise it is treated as a project directory and `/MAIN.md` is appended.
- If `<PROJECT_PATH>` is a relative path, candidates are:
  - `<cwd>/.codexpotter/projects/<PROJECT_PATH>/MAIN.md`
  - `<cwd>/<PROJECT_PATH>/MAIN.md`

The resolver requires exactly one existing file. If no candidates exist it returns an error listing
the tried paths, and if multiple candidates exist it returns an ambiguity error listing all
candidates.

See `resume.md` for how replay works and which artifacts are required.

## Differences vs. `codex exec`

- `codex-potter` uses an external `codex app-server` process, while `codex exec` runs codex-core
  directly.
- `codex-potter` renders rich TUI-formatted output (Markdown, diffs, exec blocks), while `codex exec`
  emits primarily raw text.

## Differences vs. `codex tui` (legacy)

- `codex tui` is interactive (composer, queueing, model selection, session selection, etc).
- `codex-potter` is multi-round: it prompts once, then runs a bounded number of turns and exits.

## Notes / gotchas

- `codex-potter` is a TUI app and requires a real TTY (it enters raw mode and listens for key events).
  It is not designed for piping output into files.
- Prompt shortcuts (initial composer):
  - Up/Down to recall prompt history when the input is empty (stored in `~/.codexpotter/history.jsonl`, max 500 entries).
  - ctrl+g to open an external editor (requires `$VISUAL` or `$EDITOR`), the same as codex.
- Thinking / reasoning events are intentionally filtered and not rendered.
- The global gitignore prompt can be disabled by setting
  `notice.hide_gitignore_prompt = true` in `~/.codexpotter/config.toml`.
- `--yolo` (`--dangerously-bypass-approvals-and-sandbox`) is unsafe: it disables Codex approvals and
  sandboxing, and `codex-potter` will also request `sandbox: "danger-full-access"` for the thread.
- The client requests `approvalPolicy: "never"` when starting the thread, and `codex-potter` is
  non-interactive. If an app-server requests an approval anyway, the current implementation will
  auto-accept to avoid hanging.
