// Forbid accidental stdout/stderr writes in the library portion of the TUI.
#![deny(clippy::print_stdout, clippy::print_stderr)]

mod exit;

mod ansi_escape;
mod app_event;
mod app_event_sender;
mod app_server_render;
mod bottom_pane;
mod clipboard_paste;
mod codex_config;
mod color;
mod custom_terminal;
mod diff_render;
mod exec_cell;
mod exec_command;
mod external_editor;
mod external_editor_integration;
mod file_search;
mod global_gitignore_prompt;
mod history_cell;
mod insert_history;
mod key_hint;
mod markdown;
mod markdown_render;
mod markdown_stream;
mod potter_tui;
mod render;
mod shimmer;
mod startup_banner;
mod status_indicator_widget;
mod streaming;
mod style;
mod terminal_cleanup;
mod terminal_palette;
mod text_formatting;
mod token_format;
mod tui;
mod ui_colors;
mod ui_consts;
mod wrapping;

#[cfg(test)]
mod test_backend;

pub use exit::AppExitInfo;
pub use exit::ExitReason;
pub use global_gitignore_prompt::GlobalGitignorePromptOutcome;
pub use global_gitignore_prompt::run_global_gitignore_prompt;
pub use potter_tui::CodexPotterTui;

pub use markdown_render::render_markdown_text;
