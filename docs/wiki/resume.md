# Resume (`codex-potter resume`)

`codex-potter resume <PROJECT_PATH>` replays a previous CodexPotter project's history and then
prompts for a follow-up action (currently: **Iterate 10 more rounds**).

The implementation is intentionally conservative:

- Replay is **read-only**: it never re-runs tools or executes commands.
- History is based on durable logs:
  - Potter-specific boundaries from `potter-rollout.jsonl`
  - Upstream app-server event logs from `rollout-*.jsonl`
- Ordering is preserved by scanning JSONL files in the recorded append order. Reordering is treated
  as corruption and surfaces as an explicit error.

## CLI usage

```sh
codex-potter resume <PROJECT_PATH>
```

`PROJECT_PATH` is resolved to a unique progress file (`.../MAIN.md`). See `cli.md` for the full
resolution algorithm.

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

- `Iterate 10 more rounds`

## Continuing after replay

When the user selects "Iterate 10 more rounds", CodexPotter continues running additional rounds on
the **same** project directory and appends new entries to `potter-rollout.jsonl`.

Key behavior:

- The progress file front matter is updated first: `finite_incantatem` is reset to `false` so the
  normal runner does not stop immediately after the next round.
- The continue budget is fixed to 10 rounds, counted from the resume action.
- `potter-rollout.jsonl` is append-only; `session_started` is not written again.
- New upstream rollouts are started via fresh app-server threads, just like a normal session.

There is no explicit locking. Concurrent runs against the same project directory are unsupported
and may corrupt the append-only logs; corruption is expected to be detected during replay and to
surface as an explicit error rather than being silently ignored.

