//! Syntax highlighting engine for the TUI.
//!
//! Wraps [syntect] with the [two_face] grammar and theme bundles to provide
//! ~250-language syntax highlighting.
//!
//! Note: unlike upstream Codex TUI, codex-potter currently does not expose a
//! user-configurable syntax theme picker/override (or custom `.tmTheme` themes);
//! it always uses the adaptive default embedded theme (Catppuccin Latte/Mocha)
//! based on terminal background.
//!
//! **Guardrails:** inputs exceeding 512 KB or 10 000 lines are rejected early
//! (returns `None`) to prevent pathological CPU/memory usage. Callers must
//! fall back to plain unstyled text.

use ratatui::style::Color as RtColor;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use std::sync::OnceLock;
use std::sync::RwLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::FontStyle;
use syntect::highlighting::Style as SyntectStyle;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxReference;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use two_face::theme::EmbeddedThemeName;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<RwLock<Theme>> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(two_face::syntax::extra_newlines)
}

fn adaptive_default_embedded_theme_name() -> EmbeddedThemeName {
    match crate::terminal_palette::default_bg() {
        Some(bg) if crate::color::is_light(bg) => EmbeddedThemeName::CatppuccinLatte,
        _ => EmbeddedThemeName::CatppuccinMocha,
    }
}

fn build_default_theme() -> Theme {
    let theme_set = two_face::theme::extra();
    theme_set
        .get(adaptive_default_embedded_theme_name())
        .clone()
}

fn theme_lock() -> &'static RwLock<Theme> {
    THEME.get_or_init(|| RwLock::new(build_default_theme()))
}

// -- Style conversion (syntect -> ratatui) ------------------------------------

/// Convert a syntect `Style` to a ratatui `Style`.
///
/// Syntax highlighting themes inherently produce RGB colors, so we allow
/// `Color::Rgb` here despite the project-wide preference for ANSI colors.
#[allow(clippy::disallowed_methods)]
fn convert_style(syn_style: SyntectStyle) -> Style {
    let mut rt_style = Style::default();

    // Map foreground color when visible.
    let fg = syn_style.foreground;
    if fg.a > 0 {
        rt_style = rt_style.fg(RtColor::Rgb(fg.r, fg.g, fg.b));
    }
    // Intentionally skip background to avoid overwriting terminal bg.

    if syn_style.font_style.contains(FontStyle::BOLD) {
        rt_style.add_modifier |= Modifier::BOLD;
    }
    // Intentionally skip italic — many terminals render it poorly or not at all.
    // Intentionally skip underline — themes like Dracula use underline on type
    // scopes (entity.name.type, support.class) which produces distracting
    // underlines on type/module names in terminal output.

    rt_style
}

// -- Syntax lookup ------------------------------------------------------------

/// Try to find a syntect `SyntaxReference` for the given language identifier.
///
/// two-face's extended syntax set (~250 languages) resolves most names and
/// extensions directly. We only patch the few aliases it cannot handle.
fn find_syntax(lang: &str) -> Option<&'static SyntaxReference> {
    let ss = syntax_set();

    // Aliases that two-face does not resolve on its own.
    let patched = match lang {
        "csharp" | "c-sharp" => "c#",
        "golang" => "go",
        "python3" => "python",
        "shell" => "bash",
        _ => lang,
    };

    // Try by token (matches file_extensions case-insensitively).
    if let Some(s) = ss.find_syntax_by_token(patched) {
        return Some(s);
    }
    // Try by exact syntax name (e.g. "Rust", "Python").
    if let Some(s) = ss.find_syntax_by_name(patched) {
        return Some(s);
    }
    // Try case-insensitive name match (e.g. "rust" -> "Rust").
    let lower = patched.to_ascii_lowercase();
    if let Some(s) = ss
        .syntaxes()
        .iter()
        .find(|s| s.name.to_ascii_lowercase() == lower)
    {
        return Some(s);
    }
    // Try raw input as file extension.
    if let Some(s) = ss.find_syntax_by_extension(lang) {
        return Some(s);
    }
    None
}

// -- Guardrail constants ------------------------------------------------------

const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;
const MAX_HIGHLIGHT_LINES: usize = 10_000;

// -- Core highlighting --------------------------------------------------------

fn highlight_to_line_spans(code: &str, lang: &str) -> Option<Vec<Vec<Span<'static>>>> {
    // Empty input has nothing to highlight; fall back to the plain text path
    // which correctly produces a single empty Line.
    if code.is_empty() {
        return None;
    }

    // Bail out early for oversized inputs to avoid excessive resource usage.
    // Count actual lines (not newline bytes) to avoid an off-by-one when
    // the input does not end with a newline.
    if code.len() > MAX_HIGHLIGHT_BYTES || code.lines().count() > MAX_HIGHLIGHT_LINES {
        return None;
    }

    let syntax = find_syntax(lang)?;
    let theme_guard = match theme_lock().read() {
        Ok(theme_guard) => theme_guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut h = HighlightLines::new(syntax, &theme_guard);
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();

    for line in LinesWithEndings::from(code) {
        let ranges = h.highlight_line(line, syntax_set()).ok()?;
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (style, text) in ranges {
            // Strip trailing line endings (LF and CR) since we handle line
            // breaks ourselves. CRLF inputs would otherwise leave a stray \r.
            let text = text.trim_end_matches(['\n', '\r']);
            if text.is_empty() {
                continue;
            }
            spans.push(Span::styled(text.to_string(), convert_style(style)));
        }
        if spans.is_empty() {
            spans.push(Span::raw(String::new()));
        }
        lines.push(spans);
    }

    Some(lines)
}

/// Highlight code in any supported language, returning styled ratatui `Line`s.
///
/// Falls back to plain unstyled text when the language is not recognized or the
/// input exceeds safety guardrails. Callers can always render the result
/// directly -- the fallback path produces equivalent plain-text lines.
///
/// Used by `markdown_render` for fenced code blocks and by `exec_cell` for bash
/// command highlighting.
pub fn highlight_code_to_lines(code: &str, lang: &str) -> Vec<Line<'static>> {
    if let Some(line_spans) = highlight_to_line_spans(code, lang) {
        line_spans.into_iter().map(Line::from).collect()
    } else {
        // Fallback: plain text, one Line per source line.
        // Use `lines()` instead of `split('\n')` to avoid a phantom trailing
        // empty element when the input ends with '\n' (as pulldown-cmark emits).
        let mut result: Vec<Line<'static>> =
            code.lines().map(|l| Line::from(l.to_string())).collect();
        if result.is_empty() {
            result.push(Line::from(String::new()));
        }
        result
    }
}

/// Backward-compatible wrapper for bash highlighting used by exec cells.
pub fn highlight_bash_to_lines(script: &str) -> Vec<Line<'static>> {
    highlight_code_to_lines(script, "bash")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Reconstruct plain text from highlighted Lines.
    fn reconstructed(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|sp| sp.content.clone())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn highlight_rust_has_keyword_style() {
        let code = "fn main() {}";
        let lines = highlight_code_to_lines(code, "rust");
        assert_eq!(reconstructed(&lines), code);

        // The `fn` keyword should have a non-default style (some color).
        let fn_span = lines[0].spans.iter().find(|sp| sp.content.as_ref() == "fn");
        assert!(fn_span.is_some(), "expected a span containing 'fn'");
        let style = fn_span.map(|s| s.style).unwrap_or_default();
        assert!(
            style.fg.is_some() || style.add_modifier != Modifier::empty(),
            "expected fn keyword to have non-default style, got {style:?}"
        );
    }

    #[test]
    fn highlight_unknown_lang_falls_back() {
        let code = "some random text";
        let lines = highlight_code_to_lines(code, "xyzlang");
        assert_eq!(reconstructed(&lines), code);
        // Should be plain text with no styling.
        for line in &lines {
            for span in &line.spans {
                assert_eq!(
                    span.style,
                    Style::default(),
                    "expected default style for unknown language"
                );
            }
        }
    }

    #[test]
    fn fallback_trailing_newline_no_phantom_line() {
        // pulldown-cmark sends code block text ending with '\n'.
        // The fallback path (unknown language) must not produce a phantom
        // empty trailing line from that newline.
        let code = "hello world\n";
        let lines = highlight_code_to_lines(code, "xyzlang");
        assert_eq!(
            lines.len(),
            1,
            "trailing newline should not produce phantom blank line, got {lines:?}"
        );
        assert_eq!(reconstructed(&lines), "hello world");
    }

    #[test]
    fn highlight_empty_string() {
        let lines = highlight_code_to_lines("", "rust");
        assert_eq!(lines.len(), 1);
        assert_eq!(reconstructed(&lines), "");
    }

    #[test]
    fn highlight_bash_preserves_content() {
        let script = "echo \"hello world\" && ls -la | grep foo";
        let lines = highlight_bash_to_lines(script);
        assert_eq!(reconstructed(&lines), script);
    }

    #[test]
    fn highlight_crlf_strips_carriage_return() {
        // Windows-style \r\n line endings must not leave a trailing \r in
        // span text — that would propagate into rendered code blocks.
        let code = "fn main() {\r\n    println!(\"hi\");\r\n}\r\n";
        let lines = highlight_code_to_lines(code, "rust");
        for (i, line) in lines.iter().enumerate() {
            for span in &line.spans {
                assert!(
                    !span.content.contains('\r'),
                    "line {i} span {:?} contains \\r",
                    span.content,
                );
            }
        }
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn style_conversion_correctness() {
        let syn = SyntectStyle {
            foreground: syntect::highlighting::Color {
                r: 255,
                g: 128,
                b: 0,
                a: 255,
            },
            background: syntect::highlighting::Color {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
            font_style: FontStyle::BOLD | FontStyle::ITALIC,
        };
        let rt = convert_style(syn);
        assert_eq!(rt.fg, Some(RtColor::Rgb(255, 128, 0)));
        // Background is intentionally skipped.
        assert_eq!(rt.bg, None);
        assert!(rt.add_modifier.contains(Modifier::BOLD));
        // Italic is intentionally suppressed.
        assert!(!rt.add_modifier.contains(Modifier::ITALIC));
        assert!(!rt.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn convert_style_suppresses_underline() {
        // Dracula (and other themes) set FontStyle::UNDERLINE on type scopes,
        // producing distracting underlines on type names in terminal output.
        // convert_style must suppress underline, just like it suppresses italic.
        let syn = SyntectStyle {
            foreground: syntect::highlighting::Color {
                r: 100,
                g: 200,
                b: 150,
                a: 255,
            },
            background: syntect::highlighting::Color {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            },
            font_style: FontStyle::UNDERLINE,
        };
        let rt = convert_style(syn);
        assert!(
            !rt.add_modifier.contains(Modifier::UNDERLINED),
            "convert_style should suppress UNDERLINE from themes — \
             themes like Dracula use underline on type scopes which \
             looks wrong in terminal output"
        );
    }

    #[test]
    fn highlight_multiline_python() {
        let code = "def hello():\n    print(\"hi\")\n    return 42";
        let lines = highlight_code_to_lines(code, "python");
        assert_eq!(reconstructed(&lines), code);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn highlight_large_input_returns_none() {
        let big = "x".repeat(MAX_HIGHLIGHT_BYTES + 1);
        let result = highlight_to_line_spans(&big, "rust");
        assert!(result.is_none(), "oversized input should fall back to None");
    }

    #[test]
    fn highlight_many_lines_returns_none() {
        let many_lines = "let x = 1;\n".repeat(MAX_HIGHLIGHT_LINES + 1);
        let result = highlight_to_line_spans(&many_lines, "rust");
        assert!(result.is_none(), "too many lines should fall back to None");
    }

    #[test]
    fn highlight_many_lines_no_trailing_newline_returns_none() {
        let mut code = "let x = 1;\n".repeat(MAX_HIGHLIGHT_LINES);
        code.push_str("let x = 1;");
        assert_eq!(code.lines().count(), MAX_HIGHLIGHT_LINES + 1);
        let result = highlight_to_line_spans(&code, "rust");
        assert!(
            result.is_none(),
            "MAX_HIGHLIGHT_LINES+1 lines without trailing newline should fall back"
        );
    }

    #[test]
    fn find_syntax_resolves_languages_and_aliases() {
        let languages = [
            "javascript",
            "typescript",
            "tsx",
            "python",
            "ruby",
            "rust",
            "go",
            "c",
            "cpp",
            "yaml",
            "bash",
            "kotlin",
            "markdown",
            "sql",
            "lua",
            "zig",
            "swift",
            "java",
            "c#",
            "elixir",
            "haskell",
            "scala",
            "dart",
            "r",
            "perl",
            "php",
            "html",
            "css",
            "json",
            "toml",
            "xml",
            "dockerfile",
        ];
        for lang in languages {
            assert!(
                find_syntax(lang).is_some(),
                "find_syntax({lang:?}) returned None"
            );
        }

        let extensions = [
            "rs", "py", "js", "ts", "rb", "go", "sh", "md", "yml", "kt", "ex", "hs", "pl", "php",
            "css", "html", "cs",
        ];
        for ext in extensions {
            assert!(
                find_syntax(ext).is_some(),
                "find_syntax({ext:?}) returned None"
            );
        }

        for alias in ["csharp", "c-sharp", "golang", "python3", "shell"] {
            assert!(
                find_syntax(alias).is_some(),
                "find_syntax({alias:?}) returned None — patched alias broken"
            );
        }
    }
}
