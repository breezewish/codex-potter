# Resume (`codex-potter resume`)

`codex-potter resume [PROJECT_PATH]` replays a previous CodexPotter project's history and then
prompts for a follow-up action (continue iterating rounds). The exact action label and budget
depend on whether the last recorded round is complete or unfinished (see "Action picker").

The implementation is intentionally conservative:

- Replay is **read-only**: it never re-runs tools or executes commands.
- History is based on durable logs:
  - Potter-specific boundaries from `potter-rollout.jsonl`
  - Upstream app-server event logs from `rollout-*.jsonl`
- Ordering is preserved by scanning JSONL files in the recorded append order. Reordering is treated
  as corruption and surfaces as an explicit error.

## CLI usage

```sh
codex-potter resume [PROJECT_PATH]
```

When `PROJECT_PATH` is omitted, CodexPotter opens a picker UI listing resumable projects under
`<cwd>/.codexpotter/projects`.

Picker shortcuts:

- Navigate: `↑/↓` (or `Ctrl+P/Ctrl+N`), `PageUp/PageDown`
- Search: type to filter (matches user request, git branch, and project path), `Backspace` deletes
- Sort: `Tab` toggles `Updated` / `Created` (newest first)
- Confirm: `Enter` resumes the selected project
- Cancel: `Esc` starts a new session; `Ctrl+C` quits

When `PROJECT_PATH` is provided, it is resolved to a unique progress file (`.../MAIN.md`). See
`cli.md` for the full resolution algorithm.

## Required artifacts

A resumable project directory contains at least:

- `MAIN.md` (the progress file)
- `potter-rollout.jsonl` (the Potter replay index and boundary log)

During live runs, upstream app-server also writes `rollout-*.jsonl` files under the configured
Codex home (by default, CodexPotter configures a `~/.codexpotter/codex-compat` home). The absolute
paths to these upstream rollout files are recorded in `potter-rollout.jsonl`.

Projects created before `potter-rollout.jsonl` was introduced are currently unsupported by
`resume`.

## Replay semantics

Replay is driven by `potter-rollout.jsonl` (`cli/src/resume.rs`):

- `session_started`: injects `EventMsg::PotterSessionStarted` (once at the top).
- `round_started`: injects `EventMsg::PotterRoundStarted`.
- `round_configured`: triggers replay of the referenced upstream rollout file.
- `session_succeeded` / `round_finished`: injects summary + boundary markers.

### Unfinished rounds (EOF without `round_finished`)

`potter-rollout.jsonl` is append-only and may end in the middle of a round (e.g. after
`round_configured` but before a trailing `round_finished`). In that case, `resume` still renders
the **session started** and **round started** boundary markers *before* showing the action picker,
so the user always sees the initial prompt and round context first.

Implementation detail (`cli/src/resume.rs`): the pre-action replay for an unfinished round includes
`EventMsg::PotterRoundStarted` and a synthesized trailing `EventMsg::PotterRoundFinished` with a
`Completed` outcome so the render-only runner exits cleanly. This synthetic `PotterRoundFinished`
does not render a "round finished" history cell; it only provides a clean exit boundary for the
renderer.

Upstream rollout replay (`cli/src/resume.rs`) intentionally only replays the persisted `EventMsg`
subset:

- Only JSONL lines with `type: "event_msg"` are decoded and forwarded to the renderer.
- Other upstream rollout items are ignored.

This matches upstream behavior and avoids attempting to reconstruct higher-level tool UI events
from response items that are not persisted as `EventMsg`.

### Session configuration snapshot

During replay, the renderer may need context (e.g. `cwd` / model name) to render headers
consistently. To support this, `resume` does a best-effort scan of the upstream rollout for a
`turn_context` and `session_meta` payload and synthesizes a single `EventMsg::SessionConfigured`
event before replaying the rest of the `event_msg` items.

If the snapshot cannot be extracted, replay proceeds without a synthesized `SessionConfigured`.

## Action picker

After replay, `resume` presents a popup selection UI (`tui/src/action_picker_prompt.rs`) that
shares the same interaction model as upstream list selection popups:

- Navigate: `↑/↓`, `Ctrl+P/Ctrl+N`
- Confirm: `Enter`
- Cancel: `Esc` or `Ctrl+C`

Currently the picker contains a single action:

- When the last recorded round is complete: `Iterate N more rounds` (controlled by `--rounds`,
  default: 10)
- When the last recorded round is unfinished: `Continue & iterate M more rounds`, where `M`
  is derived from the recorded `round_current` / `round_total` in `potter-rollout.jsonl`

## Continuing after replay

When the user selects "Iterate N more rounds", CodexPotter continues running additional rounds on
the **same** project directory and appends new entries to `potter-rollout.jsonl` (`N` is controlled
by `--rounds`).

Key behavior:

- The progress file front matter is updated first: `finite_incantatem` is reset to `false` so the
  normal runner does not stop immediately after the next round.
- The continue budget is `--rounds` (default: 10) rounds, counted from the resume action.
- `potter-rollout.jsonl` is append-only; `session_started` is not written again.
- New upstream rollouts are started via fresh app-server threads, just like a normal session.

When resuming an unfinished round, CodexPotter resumes the existing upstream thread (recorded in
`potter-rollout.jsonl`) and sends a `Continue` prompt to complete the current round, then starts
fresh rounds for the remaining budget (`cli/src/round_runner.rs::continue_potter_round`).

There is no explicit locking. Concurrent runs against the same project directory are unsupported
and may corrupt the append-only logs; corruption is expected to be detected during replay and to
surface as an explicit error rather than being silently ignored.
