use std::time::Duration;
use std::time::Instant;

use super::model::CommandOutput;
use super::model::ExecCall;
use super::model::ExecCell;
use crate::ansi_escape::ansi_escape_line;
use crate::exec_command::extract_bash_command;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::history_cell::HistoryCell;
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::line_utils::prefix_lines;
use crate::render::line_utils::push_owned_lines;
use crate::shimmer::shimmer_spans;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_line;
use crate::wrapping::word_wrap_lines;
use codex_protocol::parse_command::ParsedCommand;
use codex_protocol::protocol::ExecCommandSource;
use itertools::Itertools;
use ratatui::prelude::*;
use ratatui::style::Modifier;
use ratatui::style::Stylize;
use textwrap::WordSplitter;
use unicode_width::UnicodeWidthStr;

use super::render_potter;

pub const TOOL_CALL_MAX_LINES: usize = 5;
const USER_SHELL_TOOL_CALL_MAX_LINES: usize = 50;
const MAX_INTERACTION_PREVIEW_CHARS: usize = 80;

pub struct OutputLinesParams {
    pub line_limit: usize,
    pub only_err: bool,
    pub include_angle_pipe: bool,
    pub include_prefix: bool,
}

pub fn new_active_exec_command(
    call_id: String,
    command: Vec<String>,
    parsed: Vec<ParsedCommand>,
    source: ExecCommandSource,
    interaction_input: Option<String>,
    animations_enabled: bool,
) -> ExecCell {
    ExecCell::new(
        ExecCall {
            call_id,
            command,
            parsed,
            output: None,
            source,
            start_time: Some(Instant::now()),
            duration: None,
            interaction_input,
        },
        animations_enabled,
    )
}

fn format_unified_exec_interaction(command: &[String], input: Option<&str>) -> String {
    let command_display = if let Some((_, script)) = extract_bash_command(command) {
        script.to_string()
    } else {
        command.join(" ")
    };
    match input {
        Some(data) if !data.is_empty() => {
            let preview = summarize_interaction_input(data);
            format!("Interacted with `{command_display}`, sent `{preview}`")
        }
        _ => format!("Waited for `{command_display}`"),
    }
}

fn summarize_interaction_input(input: &str) -> String {
    let single_line = input.replace('\n', "\\n");
    let sanitized = single_line.replace('`', "\\`");
    if sanitized.chars().count() <= MAX_INTERACTION_PREVIEW_CHARS {
        return sanitized;
    }

    let mut preview = String::new();
    for ch in sanitized.chars().take(MAX_INTERACTION_PREVIEW_CHARS) {
        preview.push(ch);
    }
    preview.push_str("...");
    preview
}

#[derive(Clone)]
pub struct OutputLines {
    pub lines: Vec<Line<'static>>,
    pub omitted: Option<usize>,
}

pub fn output_lines(output: Option<&CommandOutput>, params: OutputLinesParams) -> OutputLines {
    let OutputLinesParams {
        line_limit,
        only_err,
        include_angle_pipe,
        include_prefix,
    } = params;
    let CommandOutput {
        aggregated_output, ..
    } = match output {
        Some(output) if only_err && output.exit_code == 0 => {
            return OutputLines {
                lines: Vec::new(),
                omitted: None,
            };
        }
        Some(output) => output,
        None => {
            return OutputLines {
                lines: Vec::new(),
                omitted: None,
            };
        }
    };

    let src = aggregated_output;
    let lines: Vec<&str> = src.lines().collect();
    let total = lines.len();
    let mut out: Vec<Line<'static>> = Vec::new();

    let head_end = total.min(line_limit);
    for (i, raw) in lines[..head_end].iter().enumerate() {
        let mut line = ansi_escape_line(raw);
        let prefix = if !include_prefix {
            ""
        } else if i == 0 && include_angle_pipe {
            "  └ "
        } else {
            "    "
        };
        line.spans.insert(0, prefix.into());
        line.spans.iter_mut().for_each(|span| {
            span.style = span.style.add_modifier(Modifier::DIM);
        });
        out.push(line);
    }

    let show_ellipsis = total > 2 * line_limit;
    let omitted = if show_ellipsis {
        Some(total - 2 * line_limit)
    } else {
        None
    };
    if show_ellipsis {
        let omitted = total - 2 * line_limit;
        out.push(format!("… +{omitted} lines").into());
    }

    let tail_start = if show_ellipsis {
        total - line_limit
    } else {
        head_end
    };
    for raw in lines[tail_start..].iter() {
        let mut line = ansi_escape_line(raw);
        if include_prefix {
            line.spans.insert(0, "    ".into());
        }
        line.spans.iter_mut().for_each(|span| {
            span.style = span.style.add_modifier(Modifier::DIM);
        });
        out.push(line);
    }

    OutputLines {
        lines: out,
        omitted,
    }
}

pub fn spinner(start_time: Option<Instant>, animations_enabled: bool) -> Span<'static> {
    if !animations_enabled {
        return "•".dim();
    }
    let elapsed = start_time.map(|st| st.elapsed()).unwrap_or_default();
    if supports_color::on_cached(supports_color::Stream::Stdout)
        .map(|level| level.has_16m)
        .unwrap_or(false)
    {
        shimmer_spans("•")[0].clone()
    } else {
        let blink_on = (elapsed.as_millis() / 600).is_multiple_of(2);
        if blink_on { "•".into() } else { "◦".dim() }
    }
}

fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis() as i64;
    if millis < 1000 {
        format!("{millis}ms")
    } else if millis < 60_000 {
        format!("{:.2}s", millis as f64 / 1000.0)
    } else {
        let minutes = millis / 60_000;
        let seconds = (millis % 60_000) / 1000;
        format!("{minutes}m {seconds:02}s")
    }
}

impl HistoryCell for ExecCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.is_exploring_cell() {
            self.exploring_display_lines(width)
        } else {
            self.command_display_lines(width)
        }
    }

    fn desired_transcript_height(&self, width: u16) -> u16 {
        self.transcript_lines(width).len() as u16
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = vec![];
        for (i, call) in self.iter_calls().enumerate() {
            if i > 0 {
                lines.push("".into());
            }
            let script = strip_bash_lc_and_escape(&call.command);
            let highlighted_script = highlight_bash_to_lines(&script);
            let cmd_display = word_wrap_lines(
                &highlighted_script,
                RtOptions::new(width as usize)
                    .initial_indent("$ ".magenta().into())
                    .subsequent_indent("    ".into()),
            );
            lines.extend(cmd_display);

            if let Some(output) = call.output.as_ref() {
                if !call.is_unified_exec_interaction() {
                    let wrap_width = width.max(1) as usize;
                    let wrap_opts = RtOptions::new(wrap_width);
                    for unwrapped in output.formatted_output.lines().map(ansi_escape_line) {
                        let wrapped = word_wrap_line(&unwrapped, wrap_opts.clone());
                        push_owned_lines(&wrapped, &mut lines);
                    }
                }
                let duration = call
                    .duration
                    .map(format_duration)
                    .unwrap_or_else(|| "unknown".to_string());
                let mut result: Line = if output.exit_code == 0 {
                    Line::from("✓".green().bold())
                } else {
                    Line::from(vec![
                        "✗".red().bold(),
                        format!(" ({})", output.exit_code).into(),
                    ])
                };
                result.push_span(format!(" • {duration}").dim());
                lines.push(result);
            }
        }
        lines
    }
}

impl ExecCell {
    fn exploring_display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        out.push(Line::from(vec![
            if self.is_active() {
                spinner(self.active_start_time(), self.animations_enabled())
            } else {
                "•".dim()
            },
            " ".into(),
            if self.is_active() {
                "Exploring".bold()
            } else {
                "Explored".bold()
            },
        ]));

        let mut out_indented = Vec::new();
        let mut pending_reads: Vec<String> = Vec::new();
        let mut display_items: Vec<(&'static str, Vec<Span<'static>>)> = Vec::new();

        let flush_pending_reads =
            |pending_reads: &mut Vec<String>,
             display_items: &mut Vec<(&'static str, Vec<Span<'static>>)>| {
                if pending_reads.is_empty() {
                    return;
                }

                let names = pending_reads.drain(..).unique();
                display_items.push((
                    "Read",
                    Itertools::intersperse(names.map(Into::into), ", ".dim()).collect(),
                ));
            };

        for call in &self.calls {
            for parsed in &call.parsed {
                match parsed {
                    ParsedCommand::Read { name, .. } => pending_reads.push(name.clone()),
                    ParsedCommand::ListFiles { cmd, path } => {
                        flush_pending_reads(&mut pending_reads, &mut display_items);
                        display_items
                            .push(("List", vec![path.clone().unwrap_or(cmd.clone()).into()]));
                    }
                    ParsedCommand::Search { cmd, query, path } => {
                        flush_pending_reads(&mut pending_reads, &mut display_items);
                        let spans = match (query, path) {
                            (Some(q), Some(p)) => {
                                vec![q.clone().into(), " in ".dim(), p.clone().into()]
                            }
                            (Some(q), None) => vec![q.clone().into()],
                            _ => vec![cmd.clone().into()],
                        };
                        display_items.push(("Search", spans));
                    }
                    ParsedCommand::Unknown { cmd } => {
                        flush_pending_reads(&mut pending_reads, &mut display_items);
                        display_items.push(("Run", vec![cmd.clone().into()]));
                    }
                }
            }
        }
        flush_pending_reads(&mut pending_reads, &mut display_items);

        for (title, line) in display_items {
            let line = Line::from(line);
            let initial_indent = Line::from(vec![title.cyan(), " ".into()]);
            let subsequent_indent = " ".repeat(initial_indent.width()).into();
            let wrapped = word_wrap_line(
                &line,
                RtOptions::new(width as usize)
                    .initial_indent(initial_indent)
                    .subsequent_indent(subsequent_indent),
            );
            push_owned_lines(&wrapped, &mut out_indented);
        }

        out.extend(prefix_lines(out_indented, "  └ ".dim(), "    ".into()));
        out
    }

    fn command_display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let calls = self.calls.as_slice();
        if calls.len() > 1 {
            if calls
                .iter()
                .all(render_potter::should_suppress_success_ran_output)
            {
                return render_potter::coalesced_success_ran_display_lines(self, width);
            }
            panic!("Expected exactly one call in a command display cell");
        }
        let [call] = calls else {
            panic!("Expected exactly one call in a command display cell");
        };
        let layout = EXEC_DISPLAY_LAYOUT;
        let suppress_output = render_potter::should_suppress_success_ran_output(call);
        let success = call.output.as_ref().map(|o| o.exit_code == 0);
        let bullet = match success {
            Some(true) => "•".green().bold(),
            Some(false) => "•".red().bold(),
            None => spinner(call.start_time, self.animations_enabled()),
        };
        let is_interaction = call.is_unified_exec_interaction();
        let title = if is_interaction {
            ""
        } else if self.is_active() {
            "Running"
        } else if call.is_user_shell_command() {
            "You ran"
        } else {
            "Ran"
        };

        let mut header_line = if is_interaction {
            Line::from(vec![bullet.clone(), " ".into()])
        } else {
            Line::from(vec![bullet.clone(), " ".into(), title.bold(), " ".into()])
        };
        let header_prefix_width = header_line.width();

        let cmd_display = if call.is_unified_exec_interaction() {
            format_unified_exec_interaction(&call.command, call.interaction_input.as_deref())
        } else {
            strip_bash_lc_and_escape(&call.command)
        };
        let highlighted_lines = highlight_bash_to_lines(&cmd_display);

        let continuation_wrap_width = layout.command_continuation.wrap_width(width);
        let continuation_opts =
            RtOptions::new(continuation_wrap_width).word_splitter(WordSplitter::NoHyphenation);
        let mut continuation_lines: Vec<Line<'static>> = Vec::new();
        if let Some((first, rest)) = highlighted_lines.split_first() {
            let available_first_width = (width as usize).saturating_sub(header_prefix_width).max(1);
            let first_opts =
                RtOptions::new(available_first_width).word_splitter(WordSplitter::NoHyphenation);
            let mut first_wrapped: Vec<Line<'static>> = Vec::new();
            push_owned_lines(&word_wrap_line(first, first_opts), &mut first_wrapped);
            let mut first_wrapped_iter = first_wrapped.into_iter();
            if let Some(first_segment) = first_wrapped_iter.next() {
                header_line.extend(first_segment);
            }
            continuation_lines.extend(first_wrapped_iter);

            for line in rest {
                push_owned_lines(
                    &word_wrap_line(line, continuation_opts.clone()),
                    &mut continuation_lines,
                );
            }
        }

        let mut lines: Vec<Line<'static>> = vec![header_line];

        let continuation_lines = Self::limit_lines_from_start(
            &continuation_lines,
            layout.command_continuation_max_lines,
        );
        if !continuation_lines.is_empty() {
            lines.extend(prefix_lines(
                continuation_lines,
                Span::from(layout.command_continuation.initial_prefix).dim(),
                Span::from(layout.command_continuation.subsequent_prefix).dim(),
            ));
        }

        if suppress_output {
            return lines;
        }

        if let Some(output) = call.output.as_ref() {
            let line_limit = if call.is_user_shell_command() {
                USER_SHELL_TOOL_CALL_MAX_LINES
            } else {
                TOOL_CALL_MAX_LINES
            };
            let raw_output = output_lines(
                Some(output),
                OutputLinesParams {
                    line_limit,
                    only_err: false,
                    include_angle_pipe: false,
                    include_prefix: false,
                },
            );
            let display_limit = if call.is_user_shell_command() {
                USER_SHELL_TOOL_CALL_MAX_LINES
            } else {
                layout.output_max_lines
            };

            if raw_output.lines.is_empty() {
                if !call.is_unified_exec_interaction() {
                    lines.extend(prefix_lines(
                        vec![Line::from("(no output)".dim())],
                        Span::from(layout.output_block.initial_prefix).dim(),
                        Span::from(layout.output_block.subsequent_prefix),
                    ));
                }
            } else {
                // Wrap first so that truncation is applied to on-screen lines
                // rather than logical lines. This ensures that a small number
                // of very long lines cannot flood the viewport.
                let mut wrapped_output: Vec<Line<'static>> = Vec::new();
                let output_wrap_width = layout.output_block.wrap_width(width);
                let output_opts =
                    RtOptions::new(output_wrap_width).word_splitter(WordSplitter::NoHyphenation);
                for line in &raw_output.lines {
                    push_owned_lines(
                        &word_wrap_line(line, output_opts.clone()),
                        &mut wrapped_output,
                    );
                }

                let trimmed_output =
                    Self::truncate_lines_middle(&wrapped_output, display_limit, raw_output.omitted);

                if !trimmed_output.is_empty() {
                    lines.extend(prefix_lines(
                        trimmed_output,
                        Span::from(layout.output_block.initial_prefix).dim(),
                        Span::from(layout.output_block.subsequent_prefix),
                    ));
                }
            }
        }

        lines
    }

    fn limit_lines_from_start(lines: &[Line<'static>], keep: usize) -> Vec<Line<'static>> {
        if lines.len() <= keep {
            return lines.to_vec();
        }
        if keep == 0 {
            return vec![Self::ellipsis_line(lines.len())];
        }

        let mut out: Vec<Line<'static>> = lines[..keep].to_vec();
        out.push(Self::ellipsis_line(lines.len() - keep));
        out
    }

    fn truncate_lines_middle(
        lines: &[Line<'static>],
        max: usize,
        omitted_hint: Option<usize>,
    ) -> Vec<Line<'static>> {
        if max == 0 {
            return Vec::new();
        }
        if lines.len() <= max {
            return lines.to_vec();
        }
        if max == 1 {
            // Carry forward any previously omitted count and add any
            // additionally hidden content lines from this truncation.
            let base = omitted_hint.unwrap_or(0);
            // When an existing ellipsis is present, `lines` already includes
            // that single representation line; exclude it from the count of
            // additionally omitted content lines.
            let extra = lines
                .len()
                .saturating_sub(usize::from(omitted_hint.is_some()));
            let omitted = base + extra;
            return vec![Self::ellipsis_line(omitted)];
        }

        let head = (max - 1) / 2;
        let tail = max - head - 1;
        let mut out: Vec<Line<'static>> = Vec::new();

        if head > 0 {
            out.extend(lines[..head].iter().cloned());
        }

        let base = omitted_hint.unwrap_or(0);
        let additional = lines
            .len()
            .saturating_sub(head + tail)
            .saturating_sub(usize::from(omitted_hint.is_some()));
        out.push(Self::ellipsis_line(base + additional));

        if tail > 0 {
            out.extend(lines[lines.len() - tail..].iter().cloned());
        }

        out
    }

    fn ellipsis_line(omitted: usize) -> Line<'static> {
        Line::from(vec![format!("… +{omitted} lines").dim()])
    }
}

#[derive(Clone, Copy)]
struct PrefixedBlock {
    initial_prefix: &'static str,
    subsequent_prefix: &'static str,
}

impl PrefixedBlock {
    const fn new(initial_prefix: &'static str, subsequent_prefix: &'static str) -> Self {
        Self {
            initial_prefix,
            subsequent_prefix,
        }
    }

    fn wrap_width(self, total_width: u16) -> usize {
        let prefix_width = UnicodeWidthStr::width(self.initial_prefix)
            .max(UnicodeWidthStr::width(self.subsequent_prefix));
        usize::from(total_width).saturating_sub(prefix_width).max(1)
    }
}

#[derive(Clone, Copy)]
struct ExecDisplayLayout {
    command_continuation: PrefixedBlock,
    command_continuation_max_lines: usize,
    output_block: PrefixedBlock,
    output_max_lines: usize,
}

impl ExecDisplayLayout {
    const fn new(
        command_continuation: PrefixedBlock,
        command_continuation_max_lines: usize,
        output_block: PrefixedBlock,
        output_max_lines: usize,
    ) -> Self {
        Self {
            command_continuation,
            command_continuation_max_lines,
            output_block,
            output_max_lines,
        }
    }
}

const EXEC_DISPLAY_LAYOUT: ExecDisplayLayout = ExecDisplayLayout::new(
    PrefixedBlock::new("  │ ", "  │ "),
    2,
    PrefixedBlock::new("  └ ", "    "),
    5,
);

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::ExecCommandSource;
    use pretty_assertions::assert_eq;

    #[test]
    fn explored_reads_are_coalesced_across_mixed_calls() {
        use std::path::PathBuf;

        let mut cell = ExecCell::new(
            ExecCall {
                call_id: "call-1".to_string(),
                command: vec!["bash".into(), "-lc".into(), "true".into()],
                parsed: vec![
                    ParsedCommand::ListFiles {
                        cmd: "ls -la".into(),
                        path: Some(".codexpotter".into()),
                    },
                    ParsedCommand::Read {
                        cmd: "sed -n '1,200p'".into(),
                        name: "resume_design.md".into(),
                        path: PathBuf::from(".codexpotter/resume_design.md"),
                    },
                ],
                output: Some(CommandOutput {
                    exit_code: 0,
                    aggregated_output: String::new(),
                    formatted_output: String::new(),
                }),
                source: ExecCommandSource::Agent,
                start_time: None,
                duration: None,
                interaction_input: None,
            },
            false,
        );

        cell.calls.push(ExecCall {
            call_id: "call-2".to_string(),
            command: vec!["bash".into(), "-lc".into(), "true".into()],
            parsed: vec![ParsedCommand::Read {
                cmd: "sed -n '200,400p'".into(),
                name: "resume_design.md".into(),
                path: PathBuf::from(".codexpotter/resume_design.md"),
            }],
            output: Some(CommandOutput {
                exit_code: 0,
                aggregated_output: String::new(),
                formatted_output: String::new(),
            }),
            source: ExecCommandSource::Agent,
            start_time: None,
            duration: None,
            interaction_input: None,
        });

        let lines = plain_strings(&cell.exploring_display_lines(80));
        let read_lines = lines
            .iter()
            .filter(|line| line.contains("Read resume_design.md"))
            .count();
        assert_eq!(read_lines, 1);
    }

    fn plain_strings(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn finished_call(command: &str) -> ExecCall {
        ExecCall {
            call_id: "call-id".to_string(),
            command: vec!["bash".into(), "-lc".into(), command.to_string()],
            parsed: Vec::new(),
            output: Some(CommandOutput {
                exit_code: 0,
                aggregated_output: String::new(),
                formatted_output: String::new(),
            }),
            source: ExecCommandSource::Agent,
            start_time: None,
            duration: None,
            interaction_input: None,
        }
    }

    #[test]
    fn user_shell_output_is_limited_by_screen_lines() {
        // Construct a user shell exec cell whose aggregated output consists of a
        // small number of very long logical lines. These will wrap into many
        // on-screen lines at narrow widths.
        //
        // Use a short marker so it survives wrapping intact inside each
        // rendered screen line; the previous test used a marker longer than
        // the wrap width, so it was split across lines and the assertion
        // never actually saw it.
        let marker = "Z";
        let long_chunk = marker.repeat(800);
        let aggregated_output = format!("{long_chunk}\n{long_chunk}\n");

        // Baseline: how many screen lines would we get if we simply wrapped
        // all logical lines without any truncation?
        let output = CommandOutput {
            exit_code: 0,
            aggregated_output,
            formatted_output: String::new(),
        };
        let width = 20;
        let layout = EXEC_DISPLAY_LAYOUT;
        let raw_output = output_lines(
            Some(&output),
            OutputLinesParams {
                // Large enough to include all logical lines without
                // triggering the ellipsis in `output_lines`.
                line_limit: 100,
                only_err: false,
                include_angle_pipe: false,
                include_prefix: false,
            },
        );
        let output_wrap_width = layout.output_block.wrap_width(width);
        let output_opts =
            RtOptions::new(output_wrap_width).word_splitter(WordSplitter::NoHyphenation);
        let mut full_wrapped_output: Vec<Line<'static>> = Vec::new();
        for line in &raw_output.lines {
            push_owned_lines(
                &word_wrap_line(line, output_opts.clone()),
                &mut full_wrapped_output,
            );
        }
        let full_screen_lines = full_wrapped_output
            .iter()
            .filter(|line| line.spans.iter().any(|span| span.content.contains(marker)))
            .count();

        // Sanity check: this scenario should produce more screen lines than
        // the user shell per-call limit when no truncation is applied. If
        // this ever fails, the test no longer exercises the regression.
        assert!(
            full_screen_lines > USER_SHELL_TOOL_CALL_MAX_LINES,
            "expected unbounded wrapping to produce more than {USER_SHELL_TOOL_CALL_MAX_LINES} screen lines, got {full_screen_lines}",
        );

        let call = ExecCall {
            call_id: "call-id".to_string(),
            command: vec!["bash".into(), "-lc".into(), "echo long".into()],
            parsed: Vec::new(),
            output: Some(output),
            source: ExecCommandSource::UserShell,
            start_time: None,
            duration: None,
            interaction_input: None,
        };

        let cell = ExecCell::new(call, false);

        // Use a narrow width so each logical line wraps into many on-screen lines.
        let lines = cell.command_display_lines(width);

        // Count how many rendered lines contain our marker text. This approximates
        // the number of visible output "screen lines" for this command.
        let output_screen_lines = lines
            .iter()
            .filter(|line| line.spans.iter().any(|span| span.content.contains(marker)))
            .count();

        // Regression guard: previously this scenario could render hundreds of
        // wrapped lines because truncation happened before wrapping. Now the
        // truncation is applied after wrapping, so the number of visible
        // screen lines is bounded by USER_SHELL_TOOL_CALL_MAX_LINES.
        assert!(
            output_screen_lines <= USER_SHELL_TOOL_CALL_MAX_LINES,
            "expected at most {USER_SHELL_TOOL_CALL_MAX_LINES} screen lines of user shell output, got {output_screen_lines}",
        );
    }

    #[test]
    fn coalesced_success_ran_summarizes_each_call_to_one_line() {
        let mut cell = ExecCell::new(finished_call("python - <<'PY'\necho 1\nPY"), false);
        cell.calls
            .push(finished_call(&format!("echo {}", "b".repeat(200))));

        let width = 40;
        let lines = cell.command_display_lines(width);
        assert_eq!(lines.len(), 2);

        let plain = plain_strings(&lines);
        assert_eq!(plain[0], "• Ran python - <<'PY' (... 2 lines)");
        assert!(
            plain[1].starts_with("      echo "),
            "expected indented second line: {:?}",
            plain[1]
        );
        assert!(
            plain[1].ends_with("..."),
            "expected ellipsis: {:?}",
            plain[1]
        );
        assert!(lines[1].width() <= width as usize);
    }
}
