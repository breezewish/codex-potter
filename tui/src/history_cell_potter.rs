use std::path::PathBuf;

use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;

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
