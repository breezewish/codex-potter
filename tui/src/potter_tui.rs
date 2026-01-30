use codex_protocol::protocol::Event;
use codex_protocol::protocol::Op;
use std::collections::VecDeque;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

use crate::AppExitInfo;
use crate::tui;
use crate::tui::Tui;

/// `codex-potter`-specific TUI session wrapper:
/// - Reuses the legacy composer to collect the initial prompt
/// - Reuses the single-turn runner pipeline to render each turn
/// - Attempts to restore terminal state on Drop
pub struct CodexPotterTui {
    tui: Tui,
    turns_rendered: bool,
    queued_user_prompts: VecDeque<String>,
    composer_draft: Option<crate::bottom_pane::ChatComposerDraft>,
}

impl CodexPotterTui {
    /// Initialize the TUI (enter raw mode) and clear the screen.
    pub fn new() -> anyhow::Result<Self> {
        let mut terminal = tui::init()?;
        terminal.clear()?;
        Ok(Self {
            tui: Tui::new(terminal),
            turns_rendered: false,
            queued_user_prompts: VecDeque::new(),
            composer_draft: None,
        })
    }

    /// Show the global gitignore recommendation prompt using the existing terminal session.
    ///
    /// This avoids tearing down and re-initializing the terminal between prompts, which can race
    /// with crossterm's stdin reader and break subsequent cursor-position queries.
    pub async fn prompt_global_gitignore(
        &mut self,
        global_gitignore_path_display: String,
    ) -> anyhow::Result<crate::GlobalGitignorePromptOutcome> {
        let result = crate::global_gitignore_prompt::run_global_gitignore_prompt_with_tui(
            &mut self.tui,
            global_gitignore_path_display,
        )
        .await;

        // Drop and recreate the underlying crossterm EventStream so any buffered input from the
        // prompt can't leak into the next screen (e.g. the composer).
        self.tui.pause_events();
        tui::flush_terminal_input_buffer();
        self.tui.resume_events();

        result
    }

    /// Collect the user's initial prompt via the legacy composer.
    ///
    /// Returns:
    /// - `Ok(Some(prompt))`: submitted
    /// - `Ok(None)`: cancelled (Ctrl+C)
    pub async fn prompt_user(&mut self) -> anyhow::Result<Option<String>> {
        crate::app_server_render::prompt_user_with_tui(&mut self.tui).await
    }

    /// Clear current screen contents (used to remove composer remnants).
    pub fn clear(&mut self) -> anyhow::Result<()> {
        self.tui.terminal.clear()?;
        Ok(())
    }

    /// Pop the next prompt queued via the bottom composer while tasks were running.
    pub fn pop_queued_user_prompt(&mut self) -> Option<String> {
        self.queued_user_prompts.pop_front()
    }

    /// Render a single turn (single-turn runner) until the turn ends or the user interrupts.
    pub async fn render_turn(
        &mut self,
        prompt: String,
        pad_before_first_cell: bool,
        codex_op_tx: UnboundedSender<Op>,
        codex_event_rx: UnboundedReceiver<Event>,
        fatal_exit_rx: UnboundedReceiver<String>,
    ) -> anyhow::Result<AppExitInfo> {
        let options = crate::app_server_render::RenderOnlyTurnOptions {
            render_user_prompt: false,
            pad_before_first_cell,
        };
        let mut queued = std::mem::take(&mut self.queued_user_prompts);
        let mut composer_draft = self.composer_draft.take();
        let backend = crate::app_server_render::RenderOnlyBackendChannels {
            codex_op_tx,
            codex_event_rx,
            fatal_exit_rx,
        };
        let result = crate::app_server_render::run_render_only_with_tui_options_and_queue(
            &mut self.tui,
            prompt,
            options,
            backend,
            &mut queued,
            &mut composer_draft,
        )
        .await;
        self.queued_user_prompts = queued;
        self.composer_draft = composer_draft;
        self.turns_rendered = true;
        result
    }
}

impl Drop for CodexPotterTui {
    fn drop(&mut self) {
        // Best-effort: clear any leftover inline UI so the user's shell prompt is clean.
        let _ = crate::terminal_cleanup::clear_inline_viewport_for_exit(&mut self.tui.terminal);

        // Always attempt to restore the terminal, even if the caller exits early.
        let _ = tui::restore();
    }
}
