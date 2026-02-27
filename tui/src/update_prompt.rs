use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget as _;
use ratatui::style::Style;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use tokio_stream::StreamExt;
use unicode_width::UnicodeWidthStr;

use crate::render::Insets;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableExt as _;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use crate::update_action::UpdateAction;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_lines;

/// Result of running the update prompt.
pub enum UpdatePromptOutcome {
    Continue,
    RunUpdate(UpdateAction),
}

/// Runs the update prompt if an update is available and should be shown.
///
/// Returns:
/// - `Continue` when no update prompt is needed or the user dismissed it
/// - `RunUpdate` when the user chose to run the self-update command
pub async fn run_update_prompt_if_needed(tui: &mut Tui) -> anyhow::Result<UpdatePromptOutcome> {
    let Some(latest_version) = crate::updates::get_upgrade_version_for_popup() else {
        return Ok(UpdatePromptOutcome::Continue);
    };
    let Some(update_action) = crate::update_action::get_update_action() else {
        return Ok(UpdatePromptOutcome::Continue);
    };

    let mut screen = UpdatePromptScreen::new(tui.frame_requester(), latest_version, update_action);
    tui.draw(u16::MAX, |frame| {
        frame.render_widget_ref(&screen, frame.area());
    })?;

    let events = tui.event_stream();
    tokio::pin!(events);

    while !screen.is_done() {
        let Some(event) = events.next().await else {
            break;
        };
        match event {
            TuiEvent::Key(key_event) => screen.handle_key(key_event),
            TuiEvent::Paste(_) => {}
            TuiEvent::Draw => {
                tui.draw(u16::MAX, |frame| {
                    frame.render_widget_ref(&screen, frame.area());
                })?;
            }
        }
    }

    // Keep behavior consistent with other prompts: clear before returning so the next screen
    // (composer) starts cleanly.
    tui.terminal.clear()?;

    match screen.selection().unwrap_or(UpdateSelection::NotNow) {
        UpdateSelection::UpdateNow => Ok(UpdatePromptOutcome::RunUpdate(update_action)),
        UpdateSelection::NotNow => Ok(UpdatePromptOutcome::Continue),
        UpdateSelection::DontRemind => {
            if let Err(err) = crate::updates::dismiss_version(screen.latest_version()).await {
                tracing::error!("Failed to persist update dismissal: {err}");
            }
            Ok(UpdatePromptOutcome::Continue)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdateSelection {
    UpdateNow,
    NotNow,
    DontRemind,
}

impl UpdateSelection {
    fn next(self) -> Self {
        match self {
            UpdateSelection::UpdateNow => UpdateSelection::NotNow,
            UpdateSelection::NotNow => UpdateSelection::DontRemind,
            UpdateSelection::DontRemind => UpdateSelection::UpdateNow,
        }
    }

    fn prev(self) -> Self {
        match self {
            UpdateSelection::UpdateNow => UpdateSelection::DontRemind,
            UpdateSelection::NotNow => UpdateSelection::UpdateNow,
            UpdateSelection::DontRemind => UpdateSelection::NotNow,
        }
    }
}

struct UpdatePromptScreen {
    request_frame: FrameRequester,
    latest_version: String,
    update_action: UpdateAction,
    highlighted: UpdateSelection,
    selection: Option<UpdateSelection>,
}

impl UpdatePromptScreen {
    fn new(
        request_frame: FrameRequester,
        latest_version: String,
        update_action: UpdateAction,
    ) -> Self {
        Self {
            request_frame,
            latest_version,
            update_action,
            highlighted: UpdateSelection::UpdateNow,
            selection: None,
        }
    }

    fn handle_key(&mut self, key_event: KeyEvent) {
        if key_event.kind == KeyEventKind::Release {
            return;
        }
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c') | KeyCode::Char('d'))
        {
            self.select(UpdateSelection::NotNow);
            return;
        }

        match key_event.code {
            KeyCode::Up | KeyCode::Char('k') => self.set_highlight(self.highlighted.prev()),
            KeyCode::Down | KeyCode::Char('j') => self.set_highlight(self.highlighted.next()),
            KeyCode::Char('1') => self.select(UpdateSelection::UpdateNow),
            KeyCode::Char('2') => self.select(UpdateSelection::NotNow),
            KeyCode::Char('3') => self.select(UpdateSelection::DontRemind),
            KeyCode::Enter => self.select(self.highlighted),
            KeyCode::Esc => self.select(UpdateSelection::NotNow),
            _ => {}
        }
    }

    fn set_highlight(&mut self, highlight: UpdateSelection) {
        if self.highlighted != highlight {
            self.highlighted = highlight;
            self.request_frame.schedule_frame();
        }
    }

    fn select(&mut self, selection: UpdateSelection) {
        self.highlighted = selection;
        self.selection = Some(selection);
        self.request_frame.schedule_frame();
    }

    fn is_done(&self) -> bool {
        self.selection.is_some()
    }

    fn selection(&self) -> Option<UpdateSelection> {
        self.selection
    }

    fn latest_version(&self) -> &str {
        self.latest_version.as_str()
    }
}

impl WidgetRef for &UpdatePromptScreen {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let update_command = self.update_action.command_str();

        let mut column = ColumnRenderable::new();
        column.push("");
        column.push(
            Line::from(vec![
                Span::from("✨ ").bold().cyan(),
                Span::from("Update available!").bold(),
                Span::from(" "),
                Span::from(format!(
                    "{current} -> {latest}",
                    current = crate::CODEX_POTTER_VERSION,
                    latest = self.latest_version
                ))
                .dim(),
            ])
            .inset(Insets::tlbr(0, 2, 0, 0)),
        );
        column.push("");
        column.push(
            Line::from(vec![
                Span::from("Release notes: ").dim(),
                Span::from("https://github.com/breezewish/CodexPotter/releases/latest")
                    .dim()
                    .underlined(),
            ])
            .inset(Insets::tlbr(0, 2, 0, 0)),
        );
        column.push("");
        column.push(selection_option_row(
            0,
            format!("Update now (runs `{update_command}`)"),
            self.highlighted == UpdateSelection::UpdateNow,
        ));
        column.push(selection_option_row(
            1,
            "Skip".to_string(),
            self.highlighted == UpdateSelection::NotNow,
        ));
        column.push(selection_option_row(
            2,
            "Skip until next version".to_string(),
            self.highlighted == UpdateSelection::DontRemind,
        ));
        column.push("");
        column.push(
            Line::from(vec![
                Span::from("Press ").dim(),
                crate::key_hint::plain(KeyCode::Enter).into(),
                Span::from(" to continue").dim(),
            ])
            .inset(Insets::tlbr(0, 2, 0, 0)),
        );

        column.render(area, buf);
    }
}

fn selection_option_row(
    index: usize,
    text: String,
    selected: bool,
) -> crate::render::renderable::RenderableItem<'static> {
    SelectionOptionRow::new(index, text, selected).inset(Insets::tlbr(0, 2, 0, 0))
}

struct SelectionOptionRow {
    prefix: String,
    label: String,
    style: Style,
}

impl SelectionOptionRow {
    fn new(index: usize, label: String, selected: bool) -> Self {
        let number = index + 1;
        let prefix = if selected {
            format!("› {number}. ")
        } else {
            format!("  {number}. ")
        };
        let style = if selected {
            Style::default().cyan()
        } else {
            Style::default()
        };
        Self {
            prefix,
            label,
            style,
        }
    }

    fn wrapped_lines(&self, width: u16) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }

        let prefix_width = UnicodeWidthStr::width(self.prefix.as_str());
        let subsequent_indent = " ".repeat(prefix_width);
        let opts = RtOptions::new(width as usize)
            .initial_indent(Line::from(self.prefix.clone()))
            .subsequent_indent(Line::from(subsequent_indent))
            .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit);

        let label = Line::from(self.label.clone()).style(self.style);
        word_wrap_lines([label], opts)
    }
}

impl Renderable for SelectionOptionRow {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(ratatui::text::Text::from(self.wrapped_lines(area.width))).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.wrapped_lines(width).len() as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_backend::VT100Backend;
    use insta::assert_snapshot;
    use ratatui::Terminal;

    #[test]
    fn update_prompt_initial_snapshot() {
        let backend = VT100Backend::new(80, 14);
        let mut terminal = Terminal::new(backend).expect("create terminal");

        let screen = UpdatePromptScreen::new(
            FrameRequester::test_dummy(),
            "9.9.9".into(),
            UpdateAction::NpmGlobalLatest,
        );

        terminal
            .draw(|frame| {
                WidgetRef::render_ref(&&screen, frame.area(), frame.buffer_mut());
            })
            .expect("draw");

        assert_snapshot!(
            "update_prompt_initial_vt100",
            terminal.backend().vt100().screen().contents()
        );
    }
}
