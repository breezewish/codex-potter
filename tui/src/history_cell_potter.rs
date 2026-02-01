use std::path::PathBuf;
use std::time::Duration;

use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use unicode_width::UnicodeWidthStr;

use crate::history_cell::HistoryCell;
use crate::history_cell::PrefixedWrappedHistoryCell;
use crate::ui_colors::secondary_color;

pub fn new_potter_round_started(current: u32, total: u32) -> PrefixedWrappedHistoryCell {
    let text: Text<'static> = Line::from(vec![
        Span::styled(
            "CodexPotter: ",
            Style::default()
                .fg(secondary_color())
                .add_modifier(Modifier::BOLD),
        ),
        format!("iteration round {current}/{total}").into(),
    ])
    .into();
    PrefixedWrappedHistoryCell::new(text, "• ".dim(), "  ")
}

pub fn new_potter_project_hint(user_prompt_file: PathBuf) -> PrefixedWrappedHistoryCell {
    let user_prompt_file = user_prompt_file.to_string_lossy().to_string();
    let text: Text<'static> =
        Line::from(vec!["Project created: ".dim(), user_prompt_file.into()]).into();
    PrefixedWrappedHistoryCell::new(text, "  ↳ ".dim(), "    ")
}

pub fn new_potter_session_succeeded(
    rounds: u32,
    duration: Duration,
    user_prompt_file: PathBuf,
    git_commit_start: String,
    git_commit_end: String,
) -> PotterSessionSucceededCell {
    PotterSessionSucceededCell {
        rounds,
        duration,
        user_prompt_file,
        git_commit_start,
        git_commit_end,
    }
}

#[derive(Debug)]
pub struct PotterSessionSucceededCell {
    rounds: u32,
    duration: Duration,
    user_prompt_file: PathBuf,
    git_commit_start: String,
    git_commit_end: String,
}

impl HistoryCell for PotterSessionSucceededCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let elapsed = crate::status_indicator_widget::fmt_elapsed_compact(self.duration.as_secs());
        let rounds = self.rounds;
        let mut lines: Vec<Line<'static>> = vec![
            potter_project_finished_separator(width),
            Line::from(""),
            Line::from("Summary:").bold(),
            Line::from(""),
            Line::from(vec![
                "  Iterated ".into(),
                format!("{rounds} rounds").bold(),
                " in ".into(),
                elapsed.bold(),
                ".".into(),
            ]),
            Line::from(""),
            Line::from(vec![
                "  Full task history: ".into(),
                self.user_prompt_file.to_string_lossy().to_string().cyan(),
            ]),
        ];

        if !(self.git_commit_start.is_empty() && self.git_commit_end.is_empty()) {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                "  Git: ".into(),
                short_git_commit(&self.git_commit_start).cyan(),
                " -> ".into(),
                short_git_commit(&self.git_commit_end).cyan(),
            ]));
        }

        lines
    }
}

fn potter_project_finished_separator(width: u16) -> Line<'static> {
    let style = Style::default()
        .fg(secondary_color())
        .add_modifier(Modifier::BOLD);

    let title = "─ CodexPotter project is finished ─";
    let title_width = title.width();
    let line = if title_width >= width as usize {
        "─".repeat(width as usize)
    } else {
        format!(
            "{title}{}",
            "─".repeat((width as usize).saturating_sub(title_width))
        )
    };

    Line::from(line).style(style)
}

fn short_git_commit(commit: &str) -> String {
    const SHORT_SHA_LEN: usize = 7;
    if commit.len() <= SHORT_SHA_LEN {
        return commit.to_string();
    }
    commit[..SHORT_SHA_LEN].to_string()
}
