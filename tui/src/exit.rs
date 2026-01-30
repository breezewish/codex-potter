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
    Fatal(String),
}
