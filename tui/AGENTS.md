# codex-potter TUI

## Overview

This `tui/` crate is expected to match upstream Codex CLI TUI behavior and styles as closely as possible,
so that users switching between codex and codex-potter have a consistent experience.

Unless explicitly documented below, changes should preserve parity.

## Explicit Divergences

Content below lists explicit divergences in codex-potter's TUI compared to upstream codex's TUI.

When introducing new changes, first identify whether it is a divergence from upstream or it makes the code more aligned with upstream.
Divergences must be documented in places below to avoid regression when syncing changes from upstream:

- Record divergences in this file, keep words concise but clear, be specific about the new behavior.
- Record divergences in doc comments.
- Cover divergences via proper tests (unit / end to end).

### Text Box

- No `/` command picker popup.
- No `?` shortcuts overlay (treat `?` as a literal character).
- `Tab` inserts a literal tab character (`\t`) into the composer.
- Supports `$` skills picker.
- Composer placeholder text is customized.
- No `Esc`-driven UX (no backtrack priming; `Esc` only dismisses popups).
- No steer mode (always queue).
- No image pasting support.
- Bottom pane footer messages are customized.
- Better word jump by using ICU4X word segmentations.
- Prompt history is persisted under `~/.codexpotter/history.jsonl`.

### Message Items

- Reasoning messages are never rendered.
- Successful `Ran` items suppress output preview and adjacent ones are collapsed into one.
- `Explored` items are more aggressively collapsed to avoid duplicates.
- Additional codex-potter items (e.g. project creation hints, round hints, project-finished summary on success).

### Shimmer

- Round prefix is added to shimmer lines.
- Remaining context window is moved into the shimmer area.
- No `esc to interrupt` message, since we interrupt using `Ctrl-C`.

### Other differences

Behavior related

- A customized banner on startup
- Additionally shows gitignore startup hint
- No syntax theme picker/override: syntax highlighting always uses the adaptive default (Catppuccin Latte/Mocha based on terminal background).
- Multi-agent collab is transcript-only: no agent thread picker UI (no per-agent transcript view).
- No streaming "Proposed Plan" block (no plan stream controller); plan updates render only via `EventMsg::PlanUpdate`.
- Resume picker UI exists for `codex-potter resume` (no path); it uses a `User Request` column (CodexPotter projects) instead of upstream `Conversation`.
- Auto retry on errors (successful recoveries are transient-only; unrecoverable errors are surfaced).
- Customized update notification / self-update (and on-disk state under `~/.codexpotter/`).
- No desktop notifications when the terminal is unfocused.

Engineering related:

- Unneeded logics and codes in codex TUI are intentionally removed to keep code tidy and focus (codex-potter's TUI is a _subset_ of codex's TUI):
  - /command picker, `?` shortcuts overlay, /model selection
  - Rewind (esc)
  - Approval flows
  - Other interactive features not needed
  - Unneeded codes, tests and snapshots
- codex-potter explicitly forbids `pub(crate)` visibility in TUI code; only `pub` and private items are allowed.
- codex-potter does not use Bazel.

## Conventions

- TUI is stateless, should be fully driven by `EventMsg`. Codex-potter has some customized rendering logic, and they are all converted into customized `EventMsg` variants (prefixed with `Potter`), so that TUI is kept as a pure rendering module without any special logic for codex-potter.

- Test: Always use snapshot tests (without ASCII escape sequences) for TUI rendering tests, so that it is visually clear what the output looks like, unless the test or code comes from upstream codex where non-snapshot tests are used, in which case you must preserve parity.

- IMPORTANT: Isolate divergent code paths: Prefer to use a new file to isolate changed logic from upstream codex, and keep the original file as a subset of the upstream's file, if the changed logic is significant. In this way, we can easily learn what has changed from upstream, and reduce merge conflicts when syncing from upstream.

## TUI Style conventions

See `styles.md`.

## TUI code conventions

- Use concise styling helpers from ratatui’s Stylize trait.
  - Basic spans: use "text".into()
  - Styled spans: use "text".red(), "text".green(), "text".magenta(), "text".dim(), etc.
  - Prefer these over constructing styles with `Span::styled` and `Style` directly.
  - Example: patch summary file lines
    - Desired: vec!["  └ ".into(), "M".red(), " ".dim(), "tui/src/app.rs".dim()]

### TUI Styling (ratatui)

- Prefer Stylize helpers: use "text".dim(), .bold(), .cyan(), .italic(), .underlined() instead of manual Style where possible.
- Prefer simple conversions: use "text".into() for spans and vec![…].into() for lines; when inference is ambiguous (e.g., Paragraph::new/Cell::from), use Line::from(spans) or Span::from(text).
- Computed styles: if the Style is computed at runtime, using `Span::styled` is OK (`Span::from(text).set_style(style)` is also acceptable).
- Avoid hardcoded white: do not use `.white()`; prefer the default foreground (no color).
- Chaining: combine helpers by chaining for readability (e.g., url.cyan().underlined()).
- Single items: prefer "text".into(); use Line::from(text) or Span::from(text) only when the target type isn’t obvious from context, or when using .into() would require extra type annotations.
- Building lines: use vec![…].into() to construct a Line when the target type is obvious and no extra type annotations are needed; otherwise use Line::from(vec![…]).
- Avoid churn: don’t refactor between equivalent forms (Span::styled ↔ set_style, Line::from ↔ .into()) without a clear readability or functional gain; follow file‑local conventions and do not introduce type annotations solely to satisfy .into().
- Compactness: prefer the form that stays on one line after rustfmt; if only one of Line::from(vec![…]) or vec![…].into() avoids wrapping, choose that. If both wrap, pick the one with fewer wrapped lines.

### Text wrapping

- Always use textwrap::wrap to wrap plain strings.
- If you have a ratatui Line and you want to wrap it, use the helpers in tui/src/wrapping.rs, e.g. word_wrap_lines / word_wrap_line.
- If you need to indent wrapped lines, use the initial_indent / subsequent_indent options from RtOptions if you can, rather than writing custom logic.
- If you have a list of lines and you need to prefix them all with some prefix (optionally different on the first vs subsequent lines), use the `prefix_lines` helper from line_utils.
