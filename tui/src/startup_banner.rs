//! Startup banner rendering.
//!
//! # Divergence from upstream Codex TUI
//!
//! `codex-potter` renders a customized startup banner (ASCII art + directory/model line) to match
//! the codex-potter CLI experience. See `tui/AGENTS.md`.

use std::path::Path;

use ratatui::prelude::*;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

use crate::exec_command::relativize_to_home;
use crate::text_formatting::center_truncate_path;
use crate::ui_colors::orange_color;
use crate::ui_colors::secondary_color;

const POTTER_ASCII_ART: &[&str] = &[
    "                 __                                 __    __                   ",
    "                /\\ \\                               /\\ \\__/\\ \\__                ",
    "  ___    ___    \\_\\ \\     __   __  _  _____     ___\\ \\ ,_\\ \\ ,_\\    __   _ __  ",
    " /'___\\ / __`\\  /'_` \\  /'__`\\/\\ \\/'\\/\\ '__`\\  / __`\\ \\ \\/\\ \\ \\/  /'__`\\/\\`'__\\",
    "/\\ \\__//\\ \\L\\ \\/\\ \\L\\ \\/\\  __/\\/>  </\\ \\ \\L\\ \\/\\ \\L\\ \\ \\ \\_\\ \\ \\_/\\  __/\\ \\ \\/ ",
    "\\ \\____\\ \\____/\\ \\___,_\\ \\____\\/\\_/\\_\\\\ \\ ,__/\\ \\____/\\ \\__\\\\ \\__\\ \\____\\\\ \\_\\ ",
    " \\/____/\\/___/  \\/__,_ /\\/____/\\//\\/_/ \\ \\ \\/  \\/___/  \\/__/ \\/__/\\/____/ \\/_/ ",
    "                                        \\ \\_\\                                  ",
    "                                         \\/_/                                  ",
];

const ASCII_INDENT: &str = "  ";
// Bold split positions (0-based, within each ASCII art line after trimming trailing spaces).
const ASCII_BOLD_SPLIT_COLS: [usize; 9] = [52, 51, 38, 37, 37, 38, 38, 40, 41];

fn take_prefix_by_width(text: &str, max_width: usize) -> &str {
    if max_width == 0 {
        return "";
    }
    if UnicodeWidthStr::width(text) <= max_width {
        return text;
    }

    let mut used_width = 0usize;
    let mut end = 0usize;
    for (idx, ch) in text.char_indices() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used_width + ch_width > max_width {
            break;
        }
        used_width += ch_width;
        end = idx + ch.len_utf8();
    }
    &text[..end]
}

/// Build the startup banner as plain `Line`s, sized to fit within `width`.
pub fn build_startup_banner_lines(
    width: u16,
    version: &str,
    model_label: &str,
    directory: &Path,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let indent_width = UnicodeWidthStr::width(ASCII_INDENT);
    let max_width = usize::from(width);
    let banner_width = POTTER_ASCII_ART
        .iter()
        .map(|line| indent_width + UnicodeWidthStr::width(line.trim_end()))
        .max()
        .unwrap_or(indent_width)
        .min(max_width);

    for (idx, line) in POTTER_ASCII_ART.iter().enumerate() {
        let trimmed = line.trim_end();
        let split_at = ASCII_BOLD_SPLIT_COLS[idx].min(trimmed.len());

        let version_label = if idx == POTTER_ASCII_ART.len().saturating_sub(1) {
            let label = format!("v{version}");
            Some(take_prefix_by_width(label.as_str(), banner_width).to_string())
        } else {
            None
        };
        let version_width = version_label
            .as_deref()
            .map(UnicodeWidthStr::width)
            .unwrap_or(0);
        let gap_reserve = if version_width > 0 && banner_width > version_width {
            1
        } else {
            0
        };
        let max_prefix_width = banner_width.saturating_sub(version_width + gap_reserve);
        let indent_visible_width = indent_width.min(max_prefix_width);
        let remaining = max_prefix_width.saturating_sub(indent_visible_width);
        let visible = take_prefix_by_width(trimmed, remaining);

        let visible_split_at = split_at.min(visible.len());
        let (left, right) = visible.split_at(visible_split_at);

        let dim_style = Style::default().dim();
        let bold_secondary_style = Style::default().fg(secondary_color()).bold();
        let mut spans: Vec<Span<'static>> = vec![
            Span::styled(ASCII_INDENT[..indent_visible_width].to_string(), dim_style),
            Span::styled(left.to_string(), dim_style),
            Span::styled(right.to_string(), bold_secondary_style),
        ];

        if let Some(version_label) = version_label {
            let prefix_width = indent_visible_width + UnicodeWidthStr::width(visible);
            let gap = banner_width.saturating_sub(prefix_width + version_width);
            spans.push(Span::from(" ".repeat(gap)));
            spans.push(Span::from(version_label).dim());
        }

        lines.push(Line::from(spans));
    }

    lines.push(Line::from(""));

    let dir_label = "directory: ";
    let dir_prefix_width = UnicodeWidthStr::width(ASCII_INDENT) + UnicodeWidthStr::width(dir_label);
    let model_gap_width = if model_label.is_empty() { 0 } else { 2 };
    let model_label_width = UnicodeWidthStr::width(model_label);
    let dir_max_width = usize::from(width)
        .saturating_sub(dir_prefix_width + model_gap_width + model_label_width)
        .max(1);
    let dir_display = format_directory(directory, Some(dir_max_width));

    let mut directory_spans: Vec<Span<'static>> = vec![
        Span::from(ASCII_INDENT),
        Span::from(dir_label).dim(),
        Span::from(dir_display),
    ];
    if !model_label.is_empty() {
        directory_spans.push(Span::from("  "));
        directory_spans.push(Span::styled(
            model_label.to_string(),
            Style::default().fg(orange_color()).bold(),
        ));
    }
    lines.push(Line::from(directory_spans));
    lines.push(Line::from(""));

    lines
}

fn format_directory(directory: &Path, max_width: Option<usize>) -> String {
    let formatted = if let Some(rel) = relativize_to_home(directory) {
        if rel.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~{}{}", std::path::MAIN_SEPARATOR, rel.display())
        }
    } else {
        directory.display().to_string()
    };

    if let Some(max_width) = max_width {
        if max_width == 0 {
            return String::new();
        }
        if UnicodeWidthStr::width(formatted.as_str()) > max_width {
            return center_truncate_path(&formatted, max_width);
        }
    }

    formatted
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;

    fn to_plain_text(lines: &[Line<'static>]) -> String {
        let mut out = String::new();
        for (idx, line) in lines.iter().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            for span in &line.spans {
                out.push_str(span.content.as_ref());
            }
        }
        out.push('\n');
        out
    }

    #[test]
    fn startup_banner_snapshot() {
        let dir = Path::new("/Users/example/repo");
        let lines = build_startup_banner_lines(120, "0.0.1", "gpt-5.2 xhigh", dir);
        assert_snapshot!("startup_banner_snapshot", to_plain_text(&lines));
    }

    #[test]
    fn startup_banner_truncates_ascii_art_without_wrapping_and_keeps_version_visible() {
        let dir = Path::new("/Users/example/repo");
        let width: u16 = 30;
        let lines = build_startup_banner_lines(width, "0.0.1", "", dir);

        for (idx, line) in lines.iter().enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                UnicodeWidthStr::width(text.as_str()) <= usize::from(width),
                "line {idx} must fit within {width} cols: {text:?}",
            );
        }

        let version_line = &lines[POTTER_ASCII_ART.len() - 1];
        let text: String = version_line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            text.ends_with("v0.0.1"),
            "version line must end with version label: {text:?}",
        );
    }
}
