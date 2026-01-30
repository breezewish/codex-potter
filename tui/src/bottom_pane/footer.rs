//! The bottom-pane footer renders transient hints and context indicators.
//!
//! The footer is pure rendering: it formats `FooterProps` into `Line`s without mutating any state.
//! It intentionally does not decide *which* footer content should be shown; that is owned by the
//! `ChatComposer` (which selects a `FooterMode`) and by higher-level state machines like
//! `ChatWidget` (which decides when quit/interrupt is allowed).
//!
//! Some footer content is time-based rather than event-based, such as the "press again to quit"
//! hint. The owning widgets schedule redraws so time-based hints can expire even if the UI is
//! otherwise idle.
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::render::line_utils::prefix_lines;
use crate::ui_consts::FOOTER_INDENT_COLS;
use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

/// The rendering inputs for the footer area under the composer.
///
/// Callers are expected to construct `FooterProps` from higher-level state (`ChatComposer`,
/// `BottomPane`, and `ChatWidget`) and pass it to `render_footer`. The footer treats these values as
/// authoritative and does not attempt to infer missing state (for example, it does not query
/// whether a task is running).
#[derive(Clone, Copy, Debug)]
pub struct FooterProps {
    pub mode: FooterMode,
    pub esc_backtrack_hint: bool,
    /// Which key the user must press again to quit.
    ///
    /// This is rendered when `mode` is `FooterMode::QuitShortcutReminder`.
    pub quit_shortcut_key: KeyBinding,
    pub context_window_percent: Option<i64>,
    pub context_window_used_tokens: Option<i64>,
}

/// Selects which footer content is rendered.
///
/// The current mode is owned by `ChatComposer`, which may override it based on transient state
/// (for example, showing `QuitShortcutReminder` only while its timer is active).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FooterMode {
    /// Transient "press again to quit" reminder (Ctrl+C/Ctrl+D).
    QuitShortcutReminder,
    ShortcutSummary,
    EscHint,
    ContextOnly,
}

pub fn esc_hint_mode(current: FooterMode, is_task_running: bool) -> FooterMode {
    if is_task_running {
        current
    } else {
        FooterMode::EscHint
    }
}

pub fn reset_mode_after_activity(current: FooterMode) -> FooterMode {
    match current {
        FooterMode::EscHint | FooterMode::QuitShortcutReminder | FooterMode::ContextOnly => {
            FooterMode::ShortcutSummary
        }
        other => other,
    }
}

pub fn footer_height(props: FooterProps) -> u16 {
    footer_lines(props).len() as u16
}

pub fn render_footer(area: Rect, buf: &mut Buffer, props: FooterProps) {
    Paragraph::new(prefix_lines(
        footer_lines(props),
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    ))
    .render(area, buf);
}

fn footer_lines(props: FooterProps) -> Vec<Line<'static>> {
    match props.mode {
        FooterMode::QuitShortcutReminder => {
            vec![quit_shortcut_reminder_line(props.quit_shortcut_key)]
        }
        FooterMode::ShortcutSummary => {
            let line = context_window_line(
                props.context_window_percent,
                props.context_window_used_tokens,
            );
            vec![line]
        }
        FooterMode::EscHint => vec![esc_hint_line(props.esc_backtrack_hint)],
        FooterMode::ContextOnly => {
            let line = context_window_line(
                props.context_window_percent,
                props.context_window_used_tokens,
            );
            vec![line]
        }
    }
}

fn quit_shortcut_reminder_line(key: KeyBinding) -> Line<'static> {
    Line::from(vec![key.into(), " again to quit".into()]).dim()
}

fn esc_hint_line(esc_backtrack_hint: bool) -> Line<'static> {
    let esc = key_hint::plain(KeyCode::Esc);
    if esc_backtrack_hint {
        Line::from(vec![esc.into(), " again to edit previous message".into()]).dim()
    } else {
        Line::from(vec![
            esc.into(),
            " ".into(),
            esc.into(),
            " to edit previous message".into(),
        ])
        .dim()
    }
}

fn context_window_line(percent: Option<i64>, used_tokens: Option<i64>) -> Line<'static> {
    if let Some(percent) = percent {
        let percent = percent.clamp(0, 100);
        return Line::from(vec![Span::from(format!("{percent}% context left")).dim()]);
    }

    if let Some(tokens) = used_tokens {
        let used_fmt = crate::token_format::format_tokens_compact(tokens);
        return Line::from(vec![Span::from(format!("{used_fmt} used")).dim()]);
    }

    Line::from(vec![Span::from("100% context left").dim()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn snapshot_footer(name: &str, props: FooterProps) {
        let height = footer_height(props).max(1);
        let mut terminal = Terminal::new(TestBackend::new(80, height)).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, f.area().width, height);
                render_footer(area, f.buffer_mut(), props);
            })
            .unwrap();
        assert_snapshot!(name, terminal.backend());
    }

    #[test]
    fn footer_snapshots() {
        snapshot_footer(
            "footer_shortcuts_default",
            FooterProps {
                mode: FooterMode::ShortcutSummary,
                esc_backtrack_hint: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
            },
        );

        snapshot_footer(
            "footer_ctrl_c_quit",
            FooterProps {
                mode: FooterMode::QuitShortcutReminder,
                esc_backtrack_hint: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
            },
        );

        snapshot_footer(
            "footer_esc_hint_idle",
            FooterProps {
                mode: FooterMode::EscHint,
                esc_backtrack_hint: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
            },
        );

        snapshot_footer(
            "footer_esc_hint_primed",
            FooterProps {
                mode: FooterMode::EscHint,
                esc_backtrack_hint: true,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
            },
        );

        snapshot_footer(
            "footer_shortcuts_context_running",
            FooterProps {
                mode: FooterMode::ShortcutSummary,
                esc_backtrack_hint: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: Some(72),
                context_window_used_tokens: None,
            },
        );

        snapshot_footer(
            "footer_context_tokens_used",
            FooterProps {
                mode: FooterMode::ShortcutSummary,
                esc_backtrack_hint: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: Some(123_456),
            },
        );

        snapshot_footer(
            "footer_context_only",
            FooterProps {
                mode: FooterMode::ContextOnly,
                esc_backtrack_hint: false,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                context_window_percent: None,
                context_window_used_tokens: None,
            },
        );
    }
}
