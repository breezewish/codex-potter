# File Search (`@` token) and Popup Integration

`codex-potter` supports an upstream-style file search popup that is driven by `@tokens` inside the
composer. The feature is split across:

- `file-search/` (crate `codex-file-search`): fast fuzzy search over repository files
- `tui/` (crate `codex-tui`): popup UI + session orchestration + insertion into the composer

This page explains the crate boundaries, the indexing/query flow, and how a chosen match becomes
text inserted into `ChatComposer`.

## Ownership

- **Upstream-derived**: the `codex-file-search` crate and the TUI integration (`tui/src/file_search.rs`
  and the popup widgets) are forked from the upstream Codex Rust workspace (`codex-rs/`) with
  minimal changes. Prefer upstream parity when modifying behavior.
- **Potter-specific**: `codex-potter` uses the feature in a reduced, render-only UI; it does not add
  new file-search semantics.

## The `codex-file-search` crate (`file-search/`)

This crate implements "fast fuzzy file search" in two steps:

1. **Walk** the directory tree to build the corpus of candidate file paths (respecting ignore rules).
2. **Match** user queries against that corpus using a fuzzy matcher, producing a ranked top-N list.

Key API surfaces:

- `create_session(search_directory, SessionOptions, reporter)` → `FileSearchSession`
- `FileSearchSession::update_query(pattern_text)` (cheap incremental query updates)
- `FileSearchSnapshot` (top-N results + metadata) delivered via `SessionReporter::on_update`

### Indexing (walk) worker

The walker uses `ignore::WalkBuilder` to traverse `search_directory` and push every file path into
the `nucleo` index via an `Injector`:

- hidden files are included (`hidden(false)`)
- symlinks are followed (`follow_links(true)`)
- git and ignore rules are respected by default (`respect_gitignore: true`)

Paths are stored **relative** to the search directory, which is what the UI displays.

### Matching worker (query → ranked results)

The matcher owns a `nucleo::Nucleo` instance and responds to:

- query updates (`WorkSignal::QueryUpdated`)
- notifications from nucleo as the corpus changes
- the file walk completing (`WorkSignal::WalkComplete`)

On each tick, it builds a `FileSearchSnapshot`:

- `matches`: top-N `FileMatch { score, path, indices? }`
- `total_match_count`: total number of matches for this query
- `scanned_file_count`: how many files have been indexed so far
- `walk_complete`: whether traversal is finished

When `SessionOptions.compute_indices` is enabled, per-match character indices are computed for UI
highlighting (sorted and deduplicated).

## TUI orchestration: `FileSearchManager` (`tui/src/file_search.rs`)

The TUI does not call `codex-file-search::run(...)`. Instead it keeps a long-lived session per
search root:

- `FileSearchManager` owns **at most one** `FileSearchSession`.
- The session is created lazily when the user starts typing a non-empty `@query`.
- When the query becomes empty, the session is dropped (stopping work).

In `codex-potter`, the search root is the current working directory (with a best-effort fallback to
`std::env::temp_dir()`), configured by the render loop that constructs the manager.

### Event flow

`ChatComposer` publishes changes of the current `@token` as:

- `AppEvent::StartFileSearch(query)`

The render loop routes this to the manager:

- `FileSearchManager::on_user_query(query)`

The manager updates `session.update_query(&query)` and the session reporter sends snapshots back
to the UI thread as:

- `AppEvent::FileSearchResult { query, matches }`

### Stale-result protection

Because file walking and matching are asynchronous, results can arrive out-of-order.

To avoid applying stale results:

- `FileSearchManager` maintains a `session_token` that is incremented whenever a new session is
  created, and ignores reporter updates from older sessions.
- The popup also treats results as stale unless the `query` matches its `pending_query`.

## Popup UI: `FileSearchPopup` (`tui/src/bottom_pane/file_search_popup.rs`)

The popup tracks two query strings:

- `pending_query`: what the user is currently typing (the request in-flight)
- `display_query`: what the currently rendered matches correspond to

While waiting for results for `pending_query`, it renders an empty row with `"loading..."`. If the
latest query has no matches, it renders `"no matches"`.

Selection is managed via a shared `ScrollState`:

- Up/Down moves the selection (wrap-around)
- the list is clamped to `MAX_POPUP_ROWS`

## Composer integration (`tui/src/bottom_pane/chat_composer.rs`)

### When the popup opens

After each handled key, the composer runs `sync_popups()`:

- If the cursor is positioned on a token starting with `@`, the file popup is shown.
- The active query is extracted by `current_at_token(...)` and **does not** include the leading
  `@`.
- A non-empty query triggers `AppEvent::StartFileSearch(query)`.

The token extraction logic is whitespace-delimited and works even when the cursor is in the middle
of the token (the cursor does not need to be at the end of the line).

### Applying results

When the render loop receives `AppEvent::FileSearchResult`, it calls:

- `ChatComposer::on_file_search_result(query, matches)`

The composer only applies results when the *current* `@token` still starts with `query`, preventing
late results from overwriting newer searches.

### Selecting and inserting a file

When the file popup is visible:

- Up/Down / Ctrl+P / Ctrl+N: move selection
- Enter / Tab: accept selection
- Esc: dismiss popup (without inserting)

Insertion is handled by `insert_selected_path(path)`:

- Replaces the active `@token` (under the cursor) with the selected relative path.
- Appends a trailing space so the user can continue typing arguments naturally.
- If the selected path contains whitespace, it is wrapped in double quotes (unless it already
  contains a `"`), so downstream prompt parsing can treat it as one argument.
