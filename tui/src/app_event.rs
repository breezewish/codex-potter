//! Application-level events used to coordinate UI actions.

use codex_protocol::protocol::Event;
use codex_protocol::protocol::Op;

use crate::file_search_engine::FileMatch;
use crate::history_cell::HistoryCell;

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum AppEvent {
    CodexEvent(Event),
    FatalExitRequest(String),

    /// Forward an `Op` to the backend. Using an `AppEvent` for this avoids
    /// bubbling channels through layers of widgets.
    CodexOp(Op),

    /// Kick off an asynchronous file search for the given query (text after
    /// the `@`).
    StartFileSearch(String),

    /// Result of a completed asynchronous file search. The `query` echoes the
    /// original search term so the UI can decide whether the results are still
    /// relevant.
    FileSearchResult {
        query: String,
        matches: Vec<FileMatch>,
    },

    InsertHistoryCell(Box<dyn HistoryCell>),

    StartCommitAnimation,
    StopCommitAnimation,
    CommitTick,
}
