# TUI Chat Composer (State Machine Notes)

This document describes the behavior of the `ChatComposer` bottom-pane input state machine
(`tui/src/bottom_pane/chat_composer.rs`) as used by `codex-potter`.

For the broader render-only TUI design (output folding, token indicator, status header updates),
see `tui-design.md`. For the wiki index, see `README.md`.

For `@` file search (session orchestration + popup insertion), see `file-search.md`.

## Ownership

`ChatComposer` is upstream-derived (forked from the upstream Codex TUI) but used in a reduced
bottom pane tailored for `codex-potter` (prompt screen + render-only runner). When changing
behavior, prefer staying close to upstream semantics unless a potter-specific constraint requires a
divergence.

## Responsibilities

The chat composer is responsible for:

- Editing the input buffer (`TextArea`) and keeping cursor/placeholder state consistent
- Routing key events to popups (for example file search) vs the main textarea
- Turning key streams into explicit paste operations when the terminal does not provide reliable
  bracketed-paste events (PasteBurst)
- Producing explicit outcomes for submission (`Submitted` / `Queued`) vs edits (`None`)
- Handling prompt-style history recall (Up/Down and Ctrl+P/Ctrl+N)

## Key Bindings (codex-potter)

High-level behavior when no popup is visible:

- `Enter`: attempts submission. When there is text, the composer returns
  `InputResult::Queued(text)` and clears the textarea.
  - `InputResult::Submitted` is currently not produced by any key binding in `codex-potter`
    (the variant remains for compatibility with the upstream Codex TUI).
- `Tab`: inserts a literal tab character (`\t`) into the textarea (does not submit).

When the file search popup is visible:

- `Enter` / `Tab`: accept the current selection (insert path)
- `Esc`: closes the popup without modifying text

## Key Routing

High-level flow:

1. `ChatComposer::handle_key_event`
2. If a popup is visible, route to a popup handler; otherwise route to
   `ChatComposer::handle_key_event_without_popup`
3. After handling any key, call `ChatComposer::sync_popups` so popup visibility matches the latest
   text/cursor state

History navigation is treated as a special mode: while browsing history, popups are suppressed so
continued Up/Down presses are not interrupted by popup focus changes.

## Submission vs Newline

The composer differentiates between:

- **Submit**: produce `InputResult::Submitted(text)` (or `Queued(text)` when queuing is enabled)
- **Insert newline**: insert `\n` into the textarea

For paste-like bursts, Enter is treated as a newline so the burst is captured as pasted text instead
of submitting mid-burst.

## Prompt History (Up/Down, Ctrl+P/Ctrl+N)

History navigation is only activated when it is unlikely the user is trying to move the cursor:

- If the input is empty: Up/Down navigates history.
- If the input is non-empty: Up/Down navigates history **only** when:
  - Cursor is at column 0, and
  - The current text matches the last history-filled value.

When a history entry is recalled, the composer replaces the entire content and resets the cursor to
column 0.

If the user edits the recalled text or moves the cursor away from column 0, further Up/Down behave
as normal cursor movement until the input is empty again.

### Persistence (`codex-potter`)

Prompt history is stored at:

- `~/.codexpotter/history.jsonl`

Each entry is one JSON object per line:

- `{"ts": <unix_seconds>, "text": "<prompt>" }`

The file is truncated to the last 500 entries to keep reads/writes fast.

## Ctrl+C Clear Behavior

The composer provides `clear_for_ctrl_c()`:

- If the input is empty: returns `None` (caller decides whether to exit/cancel).
- If the input is non-empty:
  - Captures the current text
  - Clears the composer
  - Resets history navigation state
  - Records the captured text into prompt history (so it can be recalled immediately via Up)
