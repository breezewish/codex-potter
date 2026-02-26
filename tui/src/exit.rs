use codex_protocol::ThreadId;
use codex_protocol::protocol::TokenUsage;

#[derive(Debug, Clone)]
pub struct AppExitInfo {
    pub token_usage: TokenUsage,
    pub thread_id: Option<ThreadId>,
    pub exit_reason: ExitReason,
}

#[derive(Debug, Clone)]
pub enum ExitReason {
    Completed,
    UserRequested,
    /// The current task failed, but the CodexPotter session can continue with the next queued
    /// task.
    TaskFailed(String),
    Fatal(String),
}
