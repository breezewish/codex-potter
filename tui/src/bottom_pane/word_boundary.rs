//! Word-boundary helpers used by the composer for word-wise navigation and deletion.
//!
//! # Divergence from upstream Codex TUI
//!
//! `codex-potter` uses ICU4X word segmentation (plus a small set of additional separator
//! characters) to provide more predictable <kbd>Alt</kbd>+<kbd>←</kbd>/<kbd>→</kbd> and
//! <kbd>Alt</kbd>+<kbd>Backspace</kbd> behavior across ASCII and non-ASCII text.
//!
//! See `tui/AGENTS.md` ("Better word jump by using ICU4X word segmentations").

use icu_segmenter::WordSegmenter;
use icu_segmenter::options::WordBreakInvariantOptions;

/// ASCII punctuation treated as word separators in addition to ICU4X segmentation boundaries.
pub const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Segment {
    start: usize,
    end: usize,
    is_whitespace: bool,
}

/// Return the byte index of the start of the previous word.
pub fn beginning_of_previous_word(text: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_pos_to_char_boundary(text, cursor_pos);
    if cursor_pos == 0 {
        return 0;
    }

    let segments = segments(text);
    let Some((probe_idx, _)) = text[..cursor_pos].char_indices().next_back() else {
        return 0;
    };

    let Some(mut segment_idx) = find_segment_containing(&segments, probe_idx) else {
        return 0;
    };

    while segments[segment_idx].is_whitespace {
        if segment_idx == 0 {
            return 0;
        }
        segment_idx -= 1;
    }

    segments[segment_idx].start
}

/// Return the byte index of the end of the next word.
pub fn end_of_next_word(text: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_pos_to_char_boundary(text, cursor_pos);
    if cursor_pos >= text.len() {
        return text.len();
    }

    let segments = segments(text);
    let Some(mut segment_idx) = segments.iter().position(|s| s.end > cursor_pos) else {
        return text.len();
    };

    while segments[segment_idx].is_whitespace {
        segment_idx += 1;
        if segment_idx >= segments.len() {
            return text.len();
        }
    }

    segments[segment_idx].end
}

fn clamp_pos_to_char_boundary(text: &str, pos: usize) -> usize {
    let mut pos = pos.min(text.len());
    while pos > 0 && !text.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

fn segments(text: &str) -> Vec<Segment> {
    if text.is_empty() {
        return Vec::new();
    }

    let segmenter = WordSegmenter::new_auto(WordBreakInvariantOptions::default());

    let mut segments = Vec::new();

    let mut iter = text.char_indices();
    let Some((_, first_ch)) = iter.next() else {
        return Vec::new();
    };

    let mut run_start = 0;
    let mut run_is_whitespace = first_ch.is_whitespace();

    for (idx, ch) in iter {
        let is_whitespace = ch.is_whitespace();
        if is_whitespace == run_is_whitespace {
            continue;
        }

        push_run(
            text,
            &segmenter,
            run_start..idx,
            run_is_whitespace,
            &mut segments,
        );
        run_start = idx;
        run_is_whitespace = is_whitespace;
    }

    push_run(
        text,
        &segmenter,
        run_start..text.len(),
        run_is_whitespace,
        &mut segments,
    );

    segments
}

fn push_run(
    text: &str,
    segmenter: &icu_segmenter::WordSegmenterBorrowed<'static>,
    run: std::ops::Range<usize>,
    is_whitespace: bool,
    out: &mut Vec<Segment>,
) {
    if run.start >= run.end {
        return;
    }

    if is_whitespace {
        out.push(Segment {
            start: run.start,
            end: run.end,
            is_whitespace: true,
        });
        return;
    }

    let slice = &text[run.clone()];
    let mut breakpoints: Vec<usize> = segmenter.segment_str(slice).collect();
    if breakpoints.first().copied() != Some(0) {
        breakpoints.insert(0, 0);
    }
    if breakpoints.last().copied() != Some(slice.len()) {
        breakpoints.push(slice.len());
    }

    for w in breakpoints.windows(2) {
        let start = run.start + w[0];
        let end = run.start + w[1];
        if start >= end {
            continue;
        }
        split_by_word_separators(text, start, end, out);
    }
}

fn split_by_word_separators(text: &str, start: usize, end: usize, out: &mut Vec<Segment>) {
    let slice = &text[start..end];
    let mut seg_start = start;
    let mut current_is_separator = None;

    for (idx, ch) in slice.char_indices() {
        let is_separator = WORD_SEPARATORS.contains(ch);
        match current_is_separator {
            None => current_is_separator = Some(is_separator),
            Some(prev) if prev != is_separator => {
                out.push(Segment {
                    start: seg_start,
                    end: start + idx,
                    is_whitespace: false,
                });
                seg_start = start + idx;
                current_is_separator = Some(is_separator);
            }
            Some(_) => {}
        }
    }

    out.push(Segment {
        start: seg_start,
        end,
        is_whitespace: false,
    });
}

fn find_segment_containing(segments: &[Segment], pos: usize) -> Option<usize> {
    segments.iter().position(|s| pos >= s.start && pos < s.end)
}
