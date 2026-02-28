# Wiki Index

Recommended reading order (new contributors):

1. `repo-layout.md` - Repository layout, crate boundaries, and ownership (Codex upstream vs
   potter-specific code).
2. `upstream-parity.md` - How to keep upstream-derived modules in sync and manage divergence.
3. `core-architecture.md` - End-to-end architecture: multi-round runner, app-server bridge, render
   pipeline, and `.codexpotter/` artifacts.
4. `app-server-bridge.md` - The JSON-RPC bridge: process lifecycle, approvals, and protocol schema.
5. `progress-files-and-kb.md` - Progress file structure/semantics and KB usage conventions.
6. `config-and-conventions.md` - Where state lives, how model config is resolved, sandbox/approval
   behavior, and how to run tests/snapshots.
7. `cli.md` - `codex-potter` CLI behavior and multi-round control flow.
8. `resume.md` - `codex-potter resume`: history replay, action picker, and required artifacts.
9. `tui-design.md` - Render-only TUI behavior: bottom pane, output folding, token usage indicator,
   and status header updates.
10. `file-search.md` - How `@` file search works (session orchestration + popup insertion).
11. `tui-chat-composer.md` - Bottom-pane input state machine notes (`ChatComposer`).

## Documentation conventions

- Avoid file line numbers (they become stale quickly). Prefer crate/module paths.
- Avoid non-portable local paths. Refer to upstream as `codex-rs/` rather than a local checkout
  location.
- Be explicit about ownership:
  - **Upstream-derived**: code forked from the upstream Codex Rust workspace (`codex-rs/`). Prefer
    minimal diffs and upstream parity.
  - **Potter-specific**: orchestration/workflow logic unique to this repo.
- Avoid referencing temporary scratch notes (the knowledge base). Convert useful findings into wiki
  pages instead.
- Keep `tui/` docs focused on rendering and input behavior only; avoid mixing in runner/business
  logic.

## Scope of this wiki

This wiki aims to document:

- architecture and end-to-end runtime flows
- how `.codexpotter/` progress files and the knowledge base are used
- TUI rendering pipeline and input state machines
- configuration layering and operational conventions (sandbox/approvals)
- upstream parity guidelines
