//! Minimal bottom-pane implementation used by the single-turn runner.
//!
//! The original Codex TUI has a large interactive bottom pane (popups, approvals, etc). For
//! `codex-potter` we only need the legacy composer UX (textarea, file search, paste burst handling)
//! both for capturing the initial prompt and for queuing follow-up prompts while a turn is
//! running.

mod chat_composer;
mod chat_composer_history;
mod file_search_popup;
mod footer;
mod paste_burst;
pub mod popup_consts;
mod queued_user_messages;
mod scroll_state;
mod selection_popup_common;
mod textarea;

pub use chat_composer::ChatComposer;
pub use chat_composer::ChatComposerDraft;
pub use chat_composer::InputResult;
pub use queued_user_messages::QueuedUserMessages;

/// How long the "press again to quit" hint stays visible.
#[cfg(test)]
pub const QUIT_SHORTCUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
