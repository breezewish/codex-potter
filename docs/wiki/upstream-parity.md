# Upstream Parity and Divergence Management

This repository is a forked, trimmed, and repackaged subset of the upstream Codex Rust workspace
(`codex-rs/`). High code quality here depends on being deliberate about **what should stay in sync
with upstream** and **what is intentionally potter-specific**.

This page is a set of engineering conventions for keeping that boundary healthy over time.

## Ownership taxonomy

Use these labels consistently in docs, reviews, and PR descriptions:

- **Upstream-derived**: code that exists in upstream `codex-rs/` with the same path and broadly the
  same semantics. Prefer minimal diffs and upstream-aligned patterns.
- **Potter-specific**: code unique to this repo (multi-round orchestration, filesystem-as-memory
  workflow, non-interactive constraints).

When in doubt, start from `docs/wiki/repo-layout.md` to understand crate boundaries and entry
points.

## Practical boundary: where "potter logic" should live

The simplest rule that scales:

- Put orchestration/workflow state machines in **`cli/`**.
- Keep **`tui/`** pure rendering + input handling; do not embed runner business logic there.
- Keep shared message types in **`protocol/`**; prefer porting upstream types instead of inventing
  new ones unless the event is explicitly potter-only.

Examples of **potter-only** surfaces that are expected to diverge:

- `cli/` (crate `codex-potter-cli`) and its prompt templates under `cli/prompts/`
- potter-only protocol events in `protocol/src/protocol.rs` (session/round markers)
- `tui/src/potter_tui.rs` (terminal lifetime wrapper and potter-specific glue)

Examples of **upstream-derived** surfaces that should remain close to upstream:

- `file-search/` (crate `codex-file-search`)
- most of `tui/src/render/`, `tui/src/markdown*`, `tui/src/exec_cell/`, `tui/src/streaming/`
- the app-server protocol mirror in `cli/src/app_server_protocol/`

## How to compare with upstream (without guessing)

Avoid relying on file names or intuition. Use direct comparison against an upstream checkout.

Suggested workflows:

- Compare crates by directory:
  - `tui/` ↔ `<upstream>/codex-rs/tui/`
  - `protocol/` ↔ `<upstream>/codex-rs/protocol/`
  - `file-search/` ↔ `<upstream>/codex-rs/file-search/`
- For one-off investigations, diff specific modules before editing behavior:
  - `<upstream>/codex-rs/tui/src/bottom_pane/*` ↔ `tui/src/bottom_pane/*`

Practical advice:

- If you are about to "fix" something in an upstream-derived module, first check whether upstream
  already solved it (and whether we can port that change verbatim).
- If we must diverge, keep the divergence as a small wrapper or a clearly scoped patch rather than
  letting it spread across many modules.

## Recording divergence (make it explicit)

When a divergence is intentional, capture:

1. **Why** it is necessary (potter constraint, non-interactive mode, simplified UI, etc.)
2. **Where** it lives (crate + module path)
3. **Whether** it should eventually be upstreamed / re-synced

A lightweight way to do this is:

- add a short note in the relevant wiki page (ownership section), and/or
- add a brief comment in code only when the behavior would otherwise look like a bug

## Common divergence categories in `codex-potter`

These are patterns you will see repeatedly; treat them as design constraints:

- **Render-only TUI**: `codex-potter` uses a reduced subset of the upstream TUI and intentionally
  avoids interactive flows that require multi-turn state (session selection, approvals UI, etc.).
- **Non-interactive approvals**: the runner uses `approvalPolicy: "never"` and auto-accepts known
  approval requests to avoid hanging.
- **Filesystem-as-memory**: durable state is the repository + a progress file; each round is a
  fresh app-server thread, not a continued conversation context.

