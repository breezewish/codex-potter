# TUI Design Notes (Render-Only Potter Mode)

This page documents the `codex-potter` TUI as it is used today: a **prompt screen** plus a
**render-only** runner that displays streamed Codex events and lets the user queue follow-up tasks.

Scope:

- bottom pane (`BottomPane` / `ChatComposer`) behavior
- how render-only consumes protocol events and turns them into rendered cells
- output folding/coalescing rules ("Explored", successful "Ran")
- token usage + context window indicator
- status header updates from reasoning ("thinking") events

Non-goals:

- end-user usage guide (see `cli.md`)
- unrelated upstream Codex TUI screens (approvals UI, slash commands, etc.) that are not present in
  this repo

## High-level UI layout

Render-only mode draws an inline viewport that is conceptually split into two regions:

- **Transient area** (top of the viewport): live, not-yet-committed lines such as the coalesced
  "Explored" / successful "Ran" blocks.
- **Bottom pane** (bottom of the viewport): status indicator + queued prompts list + composer +
  prompt footer.

Implementation entry point:

- `tui/src/app_server_render.rs`: `render_render_only_viewport(...)`

## Bottom pane (`tui/src/bottom_pane/`)

### Composition

`BottomPane` is a deliberately small subset of upstream Codex's bottom pane:

- `StatusIndicatorWidget` (optional; only shown while a task is running)
- `QueuedUserMessages` list (shown while running; supports quick editing)
- `ChatComposer` textarea (always present)
- 1-line prompt footer ("ctrl+g external editor", with temporary overrides)

Code:

- `tui/src/bottom_pane/mod.rs`: `BottomPane`, `BottomPaneParams`
- `tui/src/status_indicator_widget.rs`: `StatusIndicatorWidget`
- `tui/src/bottom_pane/queued_user_messages.rs`: queue rendering (`↳ ...`, plus `Alt+Up edit`)

### Chat composer state machine

The composer owns text editing, paste-burst handling, history navigation, and popup routing.

Deep dive: `tui-chat-composer.md`.

### Queued prompts (turn is running)

When a turn is running, `Enter` queues the current composer text instead of submitting a new turn.

- Collection:
  - `tui/src/app_server_render.rs`: `RenderAppState::handle_key_event(...)`
  - `InputResult::Queued(text)` is appended to `RenderAppState::queued_user_messages`
  - `BottomPane::set_queued_user_messages(...)` refreshes the list
- Editing:
  - `Alt+Up` pops the most recently queued prompt and restores it to the composer for edits

Cross-round persistence:

- `tui/src/potter_tui.rs`: `CodexPotterTui` stores a `VecDeque<String>` of queued prompts and passes
  it into / out of the render-only runner so queued prompts survive across rounds.
- `cli/src/main.rs`: after the current session ends, queued prompts are treated as **new sessions**
  (new `.codexpotter/projects/...` directories) rather than continuing the same context.

### External editor (`ctrl+g`)

`ctrl+g` opens `$VISUAL`/`$EDITOR` and replaces the current composer contents on success.

- Code:
  - `tui/src/external_editor_integration.rs`: editor invocation
  - `tui/src/bottom_pane/mod.rs`: `PromptFooterOverride::ExternalEditorHint`
  - `tui/src/app_server_render.rs`: prompt screen + render-only runner both share the same ctrl+g
    integration path (set override, draw, run editor, apply edit, clear override).

## Render-only event -> cell pipeline

### Event consumption

Render-only mode consumes `codex_protocol::protocol::EventMsg` values and translates them into
renderable `HistoryCell`s.

- `tui/src/app_server_render.rs`:
  - `RenderOnlyProcessor`: stateful translator (streaming buffer, token usage, coalescing buffers)
  - `RenderAppState::handle_app_event(...)`: processes `AppEvent::CodexEvent(...)`

Design choices:

- "Thinking" / reasoning events are not rendered into the transcript, but may update the status
  header (see below).
- Some exec-related events are buffered and rendered transiently until it is safe to commit them to
  the transcript (see "Output folding" below).

### Streaming agent output

Agent output can arrive as either:

- `AgentMessageDelta` (streaming): handled via `StreamController`, periodically "committed" to a
  `HistoryCell` (commit animation).
- `AgentMessage` (non-streaming): rendered as a single markdown cell.

Code:

- `tui/src/streaming/`: `StreamController`
- `tui/src/markdown_stream.rs`: streaming markdown chunking/flush logic
- `tui/src/app_server_render.rs`: `RenderOnlyProcessor::handle_codex_event(...)`

## Output folding / coalescing

Render-only mode intentionally buffers some exec output to avoid writing noisy, low-value blocks
into the transcript (and because scrollback output cannot be "edited" once emitted).

### "Explored" folding (read/list/search calls)

Goal: merge many "exploring" exec calls (read/list/search) into a single compact `ExecCell`.

How it works:

- The renderer treats a subset of `ExecCommandEnd` events as "exploring calls" (non-UserShell
  source, parsed commands are a subset of `Read/ListFiles/Search`).
- These calls are accumulated into `RenderOnlyProcessor::pending_exploring_cell` instead of being
  immediately committed to the transcript.
- While pending, the exploring block is rendered in the transient area above the bottom pane.
- Rendering coalesces adjacent `Read` parsed commands across call boundaries (including *mixed*
  calls like `ListFiles` + `Read`) and deduplicates file names within each contiguous `Read` block.
  This intentionally diverges from upstream `codex-rs/tui`, which only coalesces consecutive
  read-only calls.
- The pending exploring cell is **flushed** (inserted as a single history cell) before events that
  would otherwise change ordering or insert unrelated cells (e.g., agent output, warnings, turn end).

Code:

- `tui/src/exec_cell/model.rs`: `ExecCell::is_exploring_cell()`
- `tui/src/exec_cell/render.rs`: `ExecCell::exploring_display_lines(...)` (Read coalescing +
  deduplication + tree-like `└` prefix)
- `tui/src/app_server_render.rs`: `RenderOnlyProcessor::pending_exploring_cell` +
  `flush_pending_exploring_cell()`

### Successful "Ran" folding (hide output + merge adjacent commands)

Goal: successful commands often produce noisy output; for `exit_code == 0` we default to hiding the
output preview and merging adjacent successful runs into one block.

How it works:

- Only a subset of successful `ExecCommandEnd` events are buffered:
  - `exit_code == 0`
  - not a "user shell" command
  - not a "unified exec interaction"
  (See `RenderOnlyProcessor::can_coalesce_success_ran_cell`.)
- These coalescable successful `Ran` events are buffered into
  `RenderOnlyProcessor::pending_success_ran_cell`.
- The pending "Ran" block is rendered transiently above the bottom pane (separate from "Explored").
- It is flushed before unrelated transcript inserts, similar to exploring.
- Rendering rules:
  - a single successful `Ran` renders only the header + command line (no output block)
  - multiple adjacent successful `Ran` calls are displayed as a coalesced list under one header

Code:

- `tui/src/app_server_render.rs`: `RenderOnlyProcessor::pending_success_ran_cell` +
  `flush_pending_success_ran_cell()`
- `tui/src/exec_cell/render.rs`: `ExecCell::coalesced_success_ran_display_lines(...)`

## Token usage & context window indicator

The UI uses `EventMsg::TokenCount` (and `TurnStarted`) events to keep a best-effort view of:

- total tokens used so far
- estimated tokens in the *current* context window
- model context window size (when available)

Render-only behavior:

- When `model_context_window` is known and > 0: show **percent remaining**.
- Otherwise: show **used tokens** as a raw count.

Code:

- `protocol/src/protocol.rs`: `TokenUsage::percent_of_context_window_remaining(...)`
  (uses a baseline subtraction to estimate "user-controllable" remaining %)
- `tui/src/app_server_render.rs`: `RenderAppState::update_bottom_pane_context_window()`
- `tui/src/bottom_pane/mod.rs`: `BottomPane::set_context_window(...)` wires values into the
  `StatusIndicatorWidget`

## Status header updates from reasoning events

Reasoning events are filtered from the transcript, but they are still useful for showing a live
"what is the agent doing" status.

Mechanism:

- `RenderAppState` tracks a `ReasoningStatusTracker` that accumulates reasoning deltas and extracts
  the first Markdown bold span (`**...**`) as the status header.
- On `TurnStarted`, the status header resets to `"Working"`.
- While reasoning deltas stream, the status header updates when a new bold header appears.
- Round markers (`PotterRoundStarted`) update a prefix such as `Round 2/10`.

Code:

- `tui/src/app_server_render.rs`: `ReasoningStatusTracker`, `extract_first_bold(...)`,
  `should_filter_thinking_event(...)`
- `tui/src/bottom_pane/mod.rs`: `BottomPane::update_status_header(...)` and
  `BottomPane::set_status_header_prefix(...)`

## Upstream vs potter notes (TUI)

- The rendering pipeline (`markdown_render`, `diff_render`, `exec_cell`, etc.) is upstream-derived
  and should stay close to upstream behavior.
- The potter-specific surfaces are mostly wrappers / glue:
  - `tui/src/potter_tui.rs`
  - `tui/src/app_server_render.rs` (render-only runner and potter marker events)
  - small bottom-pane reductions (removing interactive Codex TUI screens)
