use std::path::Path;

use ratatui::prelude::*;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use crate::exec_command::relativize_to_home;
use crate::text_formatting::center_truncate_path;
use crate::ui_colors::orange_color;

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

pub(crate) fn build_startup_banner_lines(
    width: u16,
    version: &str,
    model_label: &str,
    directory: &Path,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let indent_width = UnicodeWidthStr::width(ASCII_INDENT);
    let banner_width = POTTER_ASCII_ART
        .iter()
        .map(|line| indent_width + UnicodeWidthStr::width(line.trim_end()))
        .max()
        .unwrap_or(indent_width)
        .min(usize::from(width));

    for (idx, line) in POTTER_ASCII_ART.iter().enumerate() {
        let trimmed = line.trim_end();
        let split_at = ASCII_BOLD_SPLIT_COLS[idx].min(trimmed.len());
        let (left, right) = trimmed.split_at(split_at);

        let mut spans: Vec<Span<'static>> = vec![
            Span::from(ASCII_INDENT),
            Span::from(left.to_string()),
            Span::styled(right.to_string(), Style::default().bold()),
        ];

        if idx == POTTER_ASCII_ART.len().saturating_sub(1) {
            let version_label = format!("v{version}");
            let base_width = indent_width + UnicodeWidthStr::width(trimmed);
            let version_width = UnicodeWidthStr::width(version_label.as_str());
            let gap = if base_width + 1 + version_width <= banner_width {
                banner_width.saturating_sub(base_width + version_width)
            } else {
                1
            };
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
    fn startup_banner_styles_are_applied() {
        let dir = Path::new("/Users/example/repo");
        let lines = build_startup_banner_lines(80, "0.0.1", "gpt-5.2 xhigh", dir);

        for (idx, line) in lines[..POTTER_ASCII_ART.len()].iter().enumerate() {
            assert_eq!(
                line.spans[0].content.as_ref(),
                ASCII_INDENT,
                "ascii art line {idx} must be indented",
            );
            assert!(
                line.spans
                    .iter()
                    .any(|span| span.style.add_modifier.contains(Modifier::BOLD)),
                "ascii art line {idx} must include a bold span",
            );
        }

        let version_line = &lines[POTTER_ASCII_ART.len() - 1];
        let version_span = version_line.spans.last().expect("version span");
        assert_eq!(version_span.content.as_ref(), "v0.0.1");
        assert!(version_span.style.add_modifier.contains(Modifier::DIM));

        let directory_line = &lines[POTTER_ASCII_ART.len() + 1];
        assert_eq!(directory_line.spans[0].content.as_ref(), ASCII_INDENT);
        assert_eq!(directory_line.spans[1].content.as_ref(), "directory: ");
        assert!(
            directory_line.spans[1]
                .style
                .add_modifier
                .contains(Modifier::DIM)
        );

        let model_span = directory_line.spans.last().expect("model span");
        assert_eq!(model_span.content.as_ref(), "gpt-5.2 xhigh");
        assert_eq!(model_span.style.fg, Some(orange_color()));
        assert!(model_span.style.add_modifier.contains(Modifier::BOLD));
    }
}
