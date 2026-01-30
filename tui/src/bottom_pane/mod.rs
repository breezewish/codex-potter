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

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Span;
use ratatui::widgets::WidgetRef;

use crate::app_event_sender::AppEventSender;
use crate::external_editor_integration;
use crate::render::renderable::Renderable;
use crate::status_indicator_widget::StatusIndicatorWidget;
use crate::tui::FrameRequester;

/// How long the "press again to quit" hint stays visible.
#[cfg(test)]
pub const QUIT_SHORTCUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptFooterOverride {
    ExternalEditorHint,
}

pub struct BottomPaneParams {
    pub frame_requester: FrameRequester,
    pub enhanced_keys_supported: bool,
    pub app_event_tx: AppEventSender,
    pub animations_enabled: bool,
    pub placeholder_text: String,
    pub disable_paste_burst: bool,
}

/// Pane displayed in the lower half of the inline viewport.
///
/// This is a minimal subset of upstream Codex's `BottomPane`: it owns the prompt input
/// (`ChatComposer`), renders queued user messages while a task is running, and optionally renders
/// a status indicator above the composer.
pub struct BottomPane {
    frame_requester: FrameRequester,
    animations_enabled: bool,

    status: Option<StatusIndicatorWidget>,
    status_header: String,
    status_header_prefix: Option<String>,
    status_details: Option<String>,
    context_window_percent: Option<i64>,
    context_window_used_tokens: Option<i64>,

    queued_user_messages: QueuedUserMessages,
    composer: ChatComposer,
    prompt_footer_override: Option<PromptFooterOverride>,
}

impl BottomPane {
    pub fn new(params: BottomPaneParams) -> Self {
        let BottomPaneParams {
            frame_requester,
            enhanced_keys_supported,
            app_event_tx,
            animations_enabled,
            placeholder_text,
            disable_paste_burst,
        } = params;

        let mut composer = ChatComposer::new(
            true,
            app_event_tx,
            enhanced_keys_supported,
            placeholder_text,
            disable_paste_burst,
        );
        composer.set_footer_hint_override(Some(Vec::new()));

        Self {
            frame_requester,
            animations_enabled,
            status: None,
            status_header: String::from("Working"),
            status_header_prefix: None,
            status_details: None,
            context_window_percent: None,
            context_window_used_tokens: None,
            queued_user_messages: QueuedUserMessages::new(),
            composer,
            prompt_footer_override: None,
        }
    }

    pub fn composer(&self) -> &ChatComposer {
        &self.composer
    }

    pub fn composer_mut(&mut self) -> &mut ChatComposer {
        &mut self.composer
    }

    pub fn set_task_running(&mut self, running: bool) {
        self.composer.set_task_running(running);

        if running {
            if self.status.is_none() {
                self.status = Some(self.new_status_indicator());
            }
            self.request_redraw();
        } else if self.status.take().is_some() {
            self.request_redraw();
        }
    }

    pub fn set_context_window(&mut self, percent: Option<i64>, used_tokens: Option<i64>) {
        if self.context_window_percent == percent && self.context_window_used_tokens == used_tokens
        {
            return;
        }

        self.context_window_percent = percent;
        self.context_window_used_tokens = used_tokens;

        if let Some(status) = self.status.as_mut() {
            status.set_context_window_visible(true);
            status.set_context_window_percent(percent);
            status.set_context_window_used_tokens(used_tokens);
        }

        self.request_redraw();
    }

    pub fn update_status_header(&mut self, header: String) {
        self.status_header = header;
        self.status_details = None;

        let header = self.status_header_with_prefix();
        let details = self.status_details.clone();
        if let Some(status) = self.status.as_mut() {
            status.update_header(header);
            status.update_details(details);
        }

        self.request_redraw();
    }

    pub fn set_status_header_prefix(&mut self, prefix: Option<String>) {
        let prefix = prefix.filter(|value| !value.is_empty());
        if self.status_header_prefix == prefix {
            return;
        }

        self.status_header_prefix = prefix;

        let header = self.status_header_with_prefix();
        if let Some(status) = self.status.as_mut() {
            status.update_header(header);
        }

        self.request_redraw();
    }

    pub fn set_queued_user_messages(&mut self, queued: Vec<String>) {
        self.queued_user_messages.messages = queued;
    }

    pub fn set_prompt_footer_override(&mut self, override_mode: Option<PromptFooterOverride>) {
        self.prompt_footer_override = override_mode;
    }

    fn status_header_with_prefix(&self) -> String {
        let Some(prefix) = self.status_header_prefix.as_deref() else {
            return self.status_header.clone();
        };

        if self.status_header.is_empty() {
            prefix.to_string()
        } else {
            format!("{prefix} · {}", self.status_header)
        }
    }

    fn new_status_indicator(&self) -> StatusIndicatorWidget {
        let mut status =
            StatusIndicatorWidget::new(self.frame_requester.clone(), self.animations_enabled);
        status.update_header(self.status_header_with_prefix());
        status.update_details(self.status_details.clone());
        status.set_interrupt_hint_visible(true);
        status.set_context_window_visible(true);
        status.set_context_window_percent(self.context_window_percent);
        status.set_context_window_used_tokens(self.context_window_used_tokens);
        status
    }

    fn request_redraw(&self) {
        self.frame_requester.schedule_frame();
    }

    #[cfg(test)]
    pub fn status_indicator(&self) -> Option<&StatusIndicatorWidget> {
        self.status.as_ref()
    }

    #[cfg(test)]
    pub fn status_indicator_mut(&mut self) -> Option<&mut StatusIndicatorWidget> {
        self.status.as_mut()
    }

    #[cfg(test)]
    pub fn context_window_percent(&self) -> Option<i64> {
        self.context_window_percent
    }

    #[cfg(test)]
    pub fn context_window_used_tokens(&self) -> Option<i64> {
        self.context_window_used_tokens
    }
}

impl Renderable for BottomPane {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        const PROMPT_FOOTER_HEIGHT: u16 = 2;
        let width = area.width;
        let prompt_footer_height = if area.height > PROMPT_FOOTER_HEIGHT {
            PROMPT_FOOTER_HEIGHT
        } else {
            0
        };

        let composer_height = self
            .composer
            .desired_height(width)
            .min(area.height.saturating_sub(prompt_footer_height));
        let composer_area = Rect::new(
            area.x,
            area.bottom()
                .saturating_sub(prompt_footer_height.saturating_add(composer_height)),
            area.width,
            composer_height,
        );
        self.composer.render(composer_area, buf);

        if prompt_footer_height > 0 {
            let footer_area = Rect::new(area.x, composer_area.bottom(), area.width, 1);
            render_prompt_footer(footer_area, buf, self.prompt_footer_override);
        }

        let height_above_composer = area
            .height
            .saturating_sub(composer_height.saturating_add(prompt_footer_height));
        if height_above_composer == 0 {
            return;
        }

        // Reserve one empty spacer line above the composer.
        let top_height = height_above_composer.saturating_sub(1);
        if top_height == 0 {
            return;
        }
        let top_area = Rect::new(area.x, area.y, area.width, top_height);

        let status_height = self
            .status
            .as_ref()
            .map(|status| status.desired_height(width).saturating_add(2))
            .unwrap_or(0)
            .min(top_area.height);
        let status_area = Rect::new(top_area.x, top_area.y, top_area.width, status_height);
        if let Some(status) = self.status.as_ref() {
            // Leave one blank line above and below the status indicator so it matches the legacy
            // Codex TUI spacing (shimmer should not touch the transcript directly).
            let available_height = status_area.height.saturating_sub(2);
            if available_height > 0 {
                let height = status
                    .desired_height(status_area.width)
                    .min(available_height);
                if height > 0 {
                    status.render(
                        Rect::new(status_area.x, status_area.y + 1, status_area.width, height),
                        buf,
                    );
                }
            }
        }

        let queue_height = top_area.height.saturating_sub(status_height);
        if queue_height == 0 {
            return;
        }
        let queue_area = Rect::new(
            top_area.x,
            top_area.y + status_height,
            top_area.width,
            queue_height,
        );
        self.queued_user_messages.render(queue_area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.status
            .as_ref()
            .map(|status| status.desired_height(width).saturating_add(2))
            .unwrap_or(0)
            + self.queued_user_messages.desired_height(width)
            + self.composer.desired_height(width)
            + 3
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        const PROMPT_FOOTER_HEIGHT: u16 = 2;
        let width = area.width;
        let prompt_footer_height = if area.height > PROMPT_FOOTER_HEIGHT {
            PROMPT_FOOTER_HEIGHT
        } else {
            0
        };
        let composer_height = self
            .composer
            .desired_height(width)
            .min(area.height.saturating_sub(prompt_footer_height));
        let composer_area = Rect::new(
            area.x,
            area.bottom()
                .saturating_sub(prompt_footer_height.saturating_add(composer_height)),
            area.width,
            composer_height,
        );
        self.composer.cursor_pos(composer_area)
    }
}

fn render_prompt_footer(area: Rect, buf: &mut Buffer, override_mode: Option<PromptFooterOverride>) {
    if area.is_empty() {
        return;
    }

    let line = match override_mode {
        Some(PromptFooterOverride::ExternalEditorHint) => ratatui::text::Line::from(vec![
            " ".into(),
            Span::from(external_editor_integration::EXTERNAL_EDITOR_HINT).bold(),
        ]),
        None => ratatui::text::Line::from(vec![
            Span::from("ctrl+g "),
            Span::from("external editor").dim(),
        ]),
    };

    // Match the legacy footer indent.
    let mut footer_rect = area;
    let indent = crate::ui_consts::LIVE_PREFIX_COLS;
    if footer_rect.width > indent {
        footer_rect.x += indent;
        footer_rect.width = footer_rect.width.saturating_sub(indent);
    }

    WidgetRef::render_ref(&line, footer_rect, buf);
}

#[cfg(test)]
pub fn render_prompt_footer_for_test(
    area: Rect,
    buf: &mut Buffer,
    override_mode: Option<PromptFooterOverride>,
) {
    render_prompt_footer(area, buf, override_mode);
}
