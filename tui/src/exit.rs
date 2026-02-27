use codex_protocol::ThreadId;
use codex_protocol::protocol::TokenUsage;

/// Summary information produced when a CodexPotter TUI session exits.
#[derive(Debug, Clone)]
pub struct AppExitInfo {
    /// Total token usage reported by the backend for the session/turn.
    pub token_usage: TokenUsage,
    /// The active thread ID, if known.
    pub thread_id: Option<ThreadId>,
    /// Why the session ended.
    pub exit_reason: ExitReason,
}

/// Reason why the CodexPotter TUI session terminated.
#[derive(Debug, Clone)]
pub enum ExitReason {
    /// The session completed normally.
    Completed,
    /// The user interrupted or requested exit.
    UserRequested,
    /// The current task failed, but the CodexPotter session can continue with the next queued
    /// task.
    TaskFailed(String),
    /// A fatal error occurred and the session cannot continue.
    Fatal(String),
}
