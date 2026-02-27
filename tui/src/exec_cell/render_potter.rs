//! CodexPotter-specific exec cell rendering helpers.
//!
//! # Divergence from upstream Codex TUI
//!
//! Upstream Codex renders output previews for successful `Ran` tool calls. `codex-potter` keeps
//! the transcript compact by suppressing output previews for successful non-user-shell commands
//! and by collapsing adjacent successful `Ran` items into one cell. See `tui/AGENTS.md`.

use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

use crate::exec_command::strip_bash_lc_and_escape;
use crate::render::highlight::highlight_bash_to_lines;

use super::model::ExecCall;
use super::model::ExecCell;

/// Return whether a successful `Ran` call should suppress its output preview.
///
/// This is applied to non-user-shell commands (tool calls) that succeeded. These calls are
/// generally used for internal automation and can be noisy, so `codex-potter` renders only the
/// command header and skips the output block.
pub fn should_suppress_success_ran_output(call: &ExecCall) -> bool {
    call.output
        .as_ref()
        .is_some_and(|output| output.exit_code == 0)
        && !call.is_user_shell_command()
        && !call.is_unified_exec_interaction()
}

/// Render header lines for a coalesced "successful Ran" cell containing multiple calls.
pub fn coalesced_success_ran_display_lines(cell: &ExecCell, width: u16) -> Vec<Line<'static>> {
    debug_assert!(
        !cell.calls.is_empty(),
        "coalesced ran cell must contain at least one call"
    );

    let [first_call, rest_calls @ ..] = cell.calls.as_slice() else {
        unreachable!("calls is non-empty");
    };

    let bullet = "•".green().bold();
    let title = "Ran";
    let mut header_line = Line::from(vec![bullet, " ".into(), title.bold(), " ".into()]);
    let header_prefix_width = header_line.width();

    let cmd_display = strip_bash_lc_and_escape(&first_call.command);
    let highlighted = highlight_bash_to_lines(&cmd_display);
    let available_width = (width as usize).saturating_sub(header_prefix_width);
    extend_line_with_command_preview(&mut header_line, &highlighted, available_width);

    let indent = " ".repeat(header_prefix_width);
    let mut lines: Vec<Line<'static>> = vec![header_line];

    for call in rest_calls {
        let cmd_display = strip_bash_lc_and_escape(&call.command);
        let highlighted = highlight_bash_to_lines(&cmd_display);
        let mut line = Line::from(vec![Span::from(indent.clone())]);
        extend_line_with_command_preview(&mut line, &highlighted, available_width);
        lines.push(line);
    }

    lines
}

fn take_line_prefix_by_width(line: &Line<'_>, max_width: usize) -> Line<'static> {
    if max_width == 0 {
        return Line::from("").style(line.style);
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used_width = 0usize;
    for span in &line.spans {
        if used_width >= max_width {
            break;
        }

        let text = span.content.as_ref();
        let mut end = 0usize;
        for (idx, ch) in text.char_indices() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used_width + ch_width > max_width {
                break;
            }
            used_width += ch_width;
            end = idx + ch.len_utf8();
        }

        if end == 0 {
            continue;
        }

        spans.push(Span::styled(text[..end].to_string(), span.style).patch_style(line.style));
    }

    Line {
        style: line.style,
        alignment: line.alignment,
        spans,
    }
}

fn truncate_line_end_with_ellipsis(line: &Line<'_>, max_width: usize) -> Line<'static> {
    const ELLIPSIS: &str = "...";

    if max_width == 0 {
        return Line::from("").style(line.style);
    }

    if line.width() <= max_width {
        return take_line_prefix_by_width(line, max_width);
    }

    if max_width <= ELLIPSIS.len() {
        return Line::from(".".repeat(max_width)).dim();
    }

    let mut truncated = take_line_prefix_by_width(line, max_width.saturating_sub(ELLIPSIS.len()));
    truncated.push_span(ELLIPSIS.dim());
    truncated
}

fn extend_line_with_command_preview(
    line: &mut Line<'static>,
    highlighted_lines: &[Line<'static>],
    available_width: usize,
) {
    let Some((first, rest)) = highlighted_lines.split_first() else {
        return;
    };

    let extra_lines = rest.len();
    let suffix = if extra_lines > 0 {
        Some(format!(" (... {extra_lines} lines)"))
    } else {
        None
    };
    let suffix_width = suffix.as_deref().map(UnicodeWidthStr::width).unwrap_or(0);
    let command_width = available_width.saturating_sub(suffix_width);

    let cmd_preview = if extra_lines > 0 {
        take_line_prefix_by_width(first, command_width)
    } else {
        truncate_line_end_with_ellipsis(first, available_width)
    };
    line.extend(cmd_preview);
    if let Some(suffix) = suffix {
        line.push_span(suffix.dim());
    }
}
