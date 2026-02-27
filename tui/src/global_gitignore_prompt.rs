//! Global gitignore recommendation prompt.
//!
//! # Divergence from upstream Codex TUI
//!
//! `codex-potter` may prompt the user on startup to add `.codexpotter/` to their global gitignore
//! so CodexPotter state files are ignored by default. Upstream Codex TUI does not show this
//! prompt. See `tui/AGENTS.md`.

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
use ratatui::text::Text;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use tokio_stream::StreamExt;
use unicode_width::UnicodeWidthStr;

use crate::render::Insets;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableExt as _;
use crate::tui;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_lines;

/// The user's selection in the global gitignore prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalGitignorePromptOutcome {
    /// Write `.codexpotter/` to the global gitignore.
    AddToGlobalGitignore,
    /// Do nothing.
    No,
    /// Do nothing and persist a config flag so we don't prompt again next time.
    NoDontAskAgain,
}

/// When the global gitignore does not ignore `.codexpotter/`, show a
/// recommendation prompt and return the user's selection.
pub async fn run_global_gitignore_prompt(
    global_gitignore_path_display: String,
) -> anyhow::Result<GlobalGitignorePromptOutcome> {
    let mut terminal = tui::init()?;
    terminal.clear()?;
    let mut tui = Tui::new(terminal);

    let result =
        run_global_gitignore_prompt_with_tui(&mut tui, global_gitignore_path_display).await;

    // Ensure the crossterm EventStream is dropped before restoring terminal modes. Otherwise it may
    // keep reading from stdin and steal cursor-position query responses from the next TUI init.
    tui.pause_events();
    tui::flush_terminal_input_buffer();

    // Always attempt to restore the terminal, even if the prompt loop fails.
    let _ = tui::restore();
    result
}

pub async fn run_global_gitignore_prompt_with_tui(
    tui: &mut Tui,
    global_gitignore_path_display: String,
) -> anyhow::Result<GlobalGitignorePromptOutcome> {
    let mut screen =
        GlobalGitignorePromptScreen::new(tui.frame_requester(), global_gitignore_path_display);
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

    // Keep behavior consistent with other prompts: clear before returning so the next UI (composer)
    // starts cleanly.
    tui.terminal.clear()?;

    Ok(match screen.selection().unwrap_or(GitignoreSelection::No) {
        GitignoreSelection::Yes => GlobalGitignorePromptOutcome::AddToGlobalGitignore,
        GitignoreSelection::No => GlobalGitignorePromptOutcome::No,
        GitignoreSelection::DontAskAgain => GlobalGitignorePromptOutcome::NoDontAskAgain,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitignoreSelection {
    Yes,
    No,
    DontAskAgain,
}

impl GitignoreSelection {
    fn next(self) -> Self {
        match self {
            GitignoreSelection::Yes => GitignoreSelection::No,
            GitignoreSelection::No => GitignoreSelection::DontAskAgain,
            GitignoreSelection::DontAskAgain => GitignoreSelection::Yes,
        }
    }

    fn prev(self) -> Self {
        match self {
            GitignoreSelection::Yes => GitignoreSelection::DontAskAgain,
            GitignoreSelection::No => GitignoreSelection::Yes,
            GitignoreSelection::DontAskAgain => GitignoreSelection::No,
        }
    }
}

struct GlobalGitignorePromptScreen {
    request_frame: FrameRequester,
    global_gitignore_path_display: String,
    highlighted: GitignoreSelection,
    selection: Option<GitignoreSelection>,
}

impl GlobalGitignorePromptScreen {
    fn new(request_frame: FrameRequester, global_gitignore_path_display: String) -> Self {
        Self {
            request_frame,
            global_gitignore_path_display,
            highlighted: GitignoreSelection::Yes,
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
            self.select(GitignoreSelection::No);
            return;
        }

        match key_event.code {
            KeyCode::Up | KeyCode::Char('k') => self.set_highlight(self.highlighted.prev()),
            KeyCode::Down | KeyCode::Char('j') => self.set_highlight(self.highlighted.next()),
            KeyCode::Char('1') => self.select(GitignoreSelection::Yes),
            KeyCode::Char('2') => self.select(GitignoreSelection::No),
            KeyCode::Char('3') => self.select(GitignoreSelection::DontAskAgain),
            KeyCode::Enter => self.select(self.highlighted),
            KeyCode::Esc => self.select(GitignoreSelection::No),
            _ => {}
        }
    }

    fn set_highlight(&mut self, highlight: GitignoreSelection) {
        if self.highlighted != highlight {
            self.highlighted = highlight;
            self.request_frame.schedule_frame();
        }
    }

    fn select(&mut self, selection: GitignoreSelection) {
        self.highlighted = selection;
        self.selection = Some(selection);
        self.request_frame.schedule_frame();
    }

    fn is_done(&self) -> bool {
        self.selection.is_some()
    }

    fn selection(&self) -> Option<GitignoreSelection> {
        self.selection
    }
}

impl WidgetRef for &GlobalGitignorePromptScreen {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let mut column = ColumnRenderable::new();
        column.push("");
        column.push(
            Line::from(vec![
                Span::from("Add "),
                Span::from(".codexpotter/").cyan(),
                Span::from(" to your global gitignore file "),
                Span::from(self.global_gitignore_path_display.clone()).cyan(),
                Span::from("?"),
            ])
            .inset(Insets::tlbr(0, 2, 0, 0)),
        );
        column.push("");
        column.push(
            Line::from(
                "This keeps your CodexPotter sessions private and prevents accidental commits.",
            )
            .dim()
            .inset(Insets::tlbr(0, 2, 0, 0)),
        );
        column.push("");
        column.push(selection_option_row(
            0,
            "Yes, add to global gitignore".to_string(),
            self.highlighted == GitignoreSelection::Yes,
        ));
        column.push(selection_option_row(
            1,
            "No".to_string(),
            self.highlighted == GitignoreSelection::No,
        ));
        column.push(selection_option_row(
            2,
            "No, don't ask me again".to_string(),
            self.highlighted == GitignoreSelection::DontAskAgain,
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
        Paragraph::new(Text::from(self.wrapped_lines(area.width))).render(area, buf);
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
    fn global_gitignore_prompt_initial_snapshot() {
        let backend = VT100Backend::new(80, 12);
        let mut terminal = Terminal::new(backend).expect("create terminal");

        let screen = GlobalGitignorePromptScreen::new(
            FrameRequester::test_dummy(),
            "~/.config/git/ignore".to_string(),
        );

        terminal
            .draw(|frame| {
                WidgetRef::render_ref(&&screen, frame.area(), frame.buffer_mut());
            })
            .expect("draw");

        assert_snapshot!(
            "global_gitignore_prompt_initial_vt100",
            terminal.backend().vt100().screen().contents()
        );
    }
}
