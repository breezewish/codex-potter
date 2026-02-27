//! CodexPotter-specific history cells.
//!
//! # Divergences from upstream Codex TUI
//!
//! Upstream Codex does not render these cells. They are used to surface CodexPotter-specific
//! runner behavior, such as multi-round iteration markers, project hints, stream recovery retries,
//! and the final "session succeeded" summary.
//!
//! See `tui/AGENTS.md` ("Additional CodexPotter items" and "auto retry on stream/network errors").

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
use crate::text_formatting::capitalize_first;
use crate::ui_colors::secondary_color;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_lines;

/// Render a marker that indicates an iteration round boundary.
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

/// Render a hint that points to the created project prompt file.
pub fn new_potter_project_hint(user_prompt_file: PathBuf) -> PrefixedWrappedHistoryCell {
    let user_prompt_file = user_prompt_file.to_string_lossy().to_string();
    let text: Text<'static> =
        Line::from(vec!["Project created: ".dim(), user_prompt_file.into()]).into();
    PrefixedWrappedHistoryCell::new(text, "  ↳ ".dim(), "    ")
}

/// Render the final multi-round summary block shown on success.
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
/// History cell rendered at the end of a successful CodexPotter session.
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
        let summary_style = Style::default()
            .fg(secondary_color())
            .add_modifier(Modifier::BOLD);

        let mut lines: Vec<Line<'static>> = vec![
            potter_session_succeeded_separator(width),
            Line::from(""),
            Line::from(vec![
                "  ".into(),
                Span::styled("CodexPotter summary:", summary_style),
                " iterated ".into(),
                format!("{rounds} rounds").bold(),
                " in ".into(),
                elapsed.bold(),
                ".".into(),
            ]),
            Line::from(""),
            Line::from(vec![
                "    Task history: ".into(),
                self.user_prompt_file.to_string_lossy().to_string().cyan(),
            ]),
        ];

        if !(self.git_commit_start.is_empty() && self.git_commit_end.is_empty()) {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                "    Git:          ".into(),
                short_git_commit(&self.git_commit_start).cyan(),
                " -> ".into(),
                short_git_commit(&self.git_commit_end).cyan(),
            ]));
        }

        lines
    }
}

fn potter_session_succeeded_separator(width: u16) -> Line<'static> {
    let style = Style::default().fg(secondary_color());
    Line::from("─".repeat(width as usize)).style(style)
}

fn short_git_commit(commit: &str) -> String {
    const SHORT_SHA_LEN: usize = 7;
    if commit.len() <= SHORT_SHA_LEN {
        return commit.to_string();
    }
    commit[..SHORT_SHA_LEN].to_string()
}

#[derive(Debug, Clone)]
/// History cell shown while CodexPotter is retrying after a stream/network error.
pub struct PotterStreamRecoveryRetryCell {
    pub attempt: u32,
    pub max_attempts: u32,
    pub error_message: String,
}

impl HistoryCell for PotterStreamRecoveryRetryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }

        let potter_style = Style::default()
            .fg(secondary_color())
            .add_modifier(Modifier::BOLD);

        let mut out = word_wrap_lines(
            [Line::from(vec![
                Span::styled("CodexPotter", potter_style),
                ": ".into(),
                format!("retry {}/{}", self.attempt, self.max_attempts).into(),
            ])],
            RtOptions::new(width.max(1) as usize)
                .initial_indent(Line::from("• ".dim()))
                .subsequent_indent(Line::from("  ")),
        );

        let error_message = capitalize_first(self.error_message.trim_start());

        let prefix = "  └ ";
        let prefix_width = UnicodeWidthStr::width(prefix);
        out.extend(word_wrap_lines(
            error_message.lines().map(|line| vec![line.dim()]),
            RtOptions::new(width.max(1) as usize)
                .initial_indent(Line::from(prefix.dim()))
                .subsequent_indent(Line::from(Span::from(" ".repeat(prefix_width)).dim()))
                .break_words(true),
        ));

        out
    }
}

#[derive(Debug, Clone)]
/// History cell shown when CodexPotter gives up retrying after stream/network errors.
pub struct PotterStreamRecoveryUnrecoverableCell {
    pub max_attempts: u32,
    pub error_message: String,
}

impl HistoryCell for PotterStreamRecoveryUnrecoverableCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }

        let potter_style = Style::default()
            .fg(secondary_color())
            .add_modifier(Modifier::BOLD);

        let mut out = word_wrap_lines(
            [Line::from(vec![
                "■ ".red(),
                Span::styled("CodexPotter", potter_style),
                ": ".red(),
                format!("unrecoverable error after {} retries", self.max_attempts).red(),
            ])],
            RtOptions::new(width.max(1) as usize).break_words(true),
        );

        let error_message = capitalize_first(self.error_message.trim_start());
        out.extend(word_wrap_lines(
            error_message.lines().map(|line| vec![line.red()]),
            RtOptions::new(width.max(1) as usize)
                .initial_indent(Line::from("  ".red()))
                .subsequent_indent(Line::from("  ".red()))
                .break_words(true),
        ));

        out
    }
}
