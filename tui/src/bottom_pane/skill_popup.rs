//! Skills picker popup.
//!
//! The popup is triggered by `$`-mentions in the composer (`ChatComposer`), and allows the user to
//! select a skill mention to insert into the input buffer.
//!
//! # Divergence from upstream Codex TUI
//!
//! `codex-potter` keeps only the `$` skills picker; it intentionally omits other upstream pickers
//! and overlays (see `tui/AGENTS.md`).

use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Widget;
use ratatui::widgets::WidgetRef;

use crate::key_hint;

use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::render_rows_single_line;

use crate::render::Insets;
use crate::render::RectExt;
use crate::text_formatting::truncate_text;

/// A selectable item shown in the `$` skills picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionItem {
    pub display_name: String,
    pub description: Option<String>,
    pub insert_text: String,
    pub search_terms: Vec<String>,
}

/// Stateful popup UI for selecting a [`MentionItem`].
pub struct SkillPopup {
    query: String,
    mentions: Vec<MentionItem>,
    state: ScrollState,
}

impl SkillPopup {
    pub fn new(mentions: Vec<MentionItem>) -> Self {
        Self {
            query: String::new(),
            mentions,
            state: ScrollState::new(),
        }
    }

    pub fn set_mentions(&mut self, mentions: Vec<MentionItem>) {
        self.mentions = mentions;
        self.clamp_selection();
    }

    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_string();
        self.clamp_selection();
    }

    pub fn calculate_required_height(&self) -> u16 {
        let rows = self.rows_from_matches(self.filtered());
        let visible = rows.len().clamp(1, MAX_POPUP_ROWS);
        (visible as u16).saturating_add(2)
    }

    pub fn move_up(&mut self) {
        let len = self.filtered_items().len();
        self.state.move_up_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    pub fn move_down(&mut self) {
        let len = self.filtered_items().len();
        self.state.move_down_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    pub fn selected_mention(&self) -> Option<&MentionItem> {
        let matches = self.filtered_items();
        let idx = self.state.selected_idx?;
        let mention_idx = matches.get(idx)?;
        self.mentions.get(*mention_idx)
    }

    fn clamp_selection(&mut self) {
        let len = self.filtered_items().len();
        self.state.clamp_selection(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    fn filtered_items(&self) -> Vec<usize> {
        self.filtered().into_iter().map(|(idx, _, _)| idx).collect()
    }

    fn rows_from_matches(
        &self,
        matches: Vec<(usize, Option<Vec<usize>>, i32)>,
    ) -> Vec<GenericDisplayRow> {
        matches
            .into_iter()
            .map(|(idx, indices, _score)| {
                let mention = &self.mentions[idx];
                let name = truncate_text(&mention.display_name, 21);
                let description = mention.description.clone().unwrap_or_default();
                GenericDisplayRow {
                    name,
                    match_indices: indices,
                    display_shortcut: None,
                    description: Some(description).filter(|desc| !desc.is_empty()),
                    is_disabled: false,
                    disabled_reason: None,
                    wrap_indent: None,
                }
            })
            .collect()
    }

    fn filtered(&self) -> Vec<(usize, Option<Vec<usize>>, i32)> {
        let filter = self.query.trim();
        let mut out: Vec<(usize, Option<Vec<usize>>, i32)> = Vec::new();

        if filter.is_empty() {
            for (idx, _mention) in self.mentions.iter().enumerate() {
                out.push((idx, None, 0));
            }
            return out;
        }

        for (idx, mention) in self.mentions.iter().enumerate() {
            let mut best_match: Option<(Option<Vec<usize>>, i32)> = None;

            if let Some((indices, score)) = fuzzy_match(&mention.display_name, filter) {
                best_match = Some((Some(indices), score));
            }

            for term in &mention.search_terms {
                if term == &mention.display_name {
                    continue;
                }

                if let Some((_indices, score)) = fuzzy_match(term, filter) {
                    match best_match.as_mut() {
                        Some((best_indices, best_score)) => {
                            if score > *best_score {
                                *best_score = score;
                                *best_indices = None;
                            }
                        }
                        None => {
                            best_match = Some((None, score));
                        }
                    }
                }
            }

            if let Some((indices, score)) = best_match {
                out.push((idx, indices, score));
            }
        }

        out.sort_by(|a, b| {
            a.2.cmp(&b.2).then_with(|| {
                let an = self.mentions[a.0].display_name.as_str();
                let bn = self.mentions[b.0].display_name.as_str();
                an.cmp(bn)
            })
        });

        out
    }
}

impl WidgetRef for &SkillPopup {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let (list_area, hint_area) = if area.height > 2 {
            let [list_area, _spacer_area, hint_area] = Layout::vertical([
                Constraint::Length(area.height - 2),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(area);
            (list_area, Some(hint_area))
        } else {
            (area, None)
        };

        let rows = self.rows_from_matches(self.filtered());
        render_rows_single_line(
            list_area.inset(Insets::tlbr(0, 2, 0, 0)),
            buf,
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            "no matches",
        );

        if let Some(hint_area) = hint_area {
            let hint_area = Rect {
                x: hint_area.x + 2,
                y: hint_area.y,
                width: hint_area.width.saturating_sub(2),
                height: hint_area.height,
            };
            skill_popup_hint_line().render(hint_area, buf);
        }
    }
}

fn skill_popup_hint_line() -> Line<'static> {
    Line::from(vec![
        "Press ".into(),
        key_hint::plain(KeyCode::Enter).into(),
        " to insert or ".into(),
        key_hint::plain(KeyCode::Esc).into(),
        " to close".into(),
    ])
}

/// Simple case-insensitive subsequence matcher used for fuzzy filtering.
///
/// Returns the indices (character positions) of the matched characters in the
/// ORIGINAL `haystack` string and a score where smaller is better.
///
/// Unicode correctness: we perform the match on a lowercased copy of the
/// haystack and needle but maintain a mapping from each character in the
/// lowercased haystack back to the original character index in `haystack`.
/// This ensures the returned indices can be safely used with
/// `str::chars().enumerate()` consumers for highlighting, even when
/// lowercasing expands certain characters.
fn fuzzy_match(haystack: &str, needle: &str) -> Option<(Vec<usize>, i32)> {
    if needle.is_empty() {
        return Some((Vec::new(), i32::MAX));
    }

    let mut lowered_chars: Vec<char> = Vec::new();
    let mut lowered_to_orig_char_idx: Vec<usize> = Vec::new();
    for (orig_idx, ch) in haystack.chars().enumerate() {
        for lc in ch.to_lowercase() {
            lowered_chars.push(lc);
            lowered_to_orig_char_idx.push(orig_idx);
        }
    }

    let lowered_needle: Vec<char> = needle.to_lowercase().chars().collect();

    let mut result_orig_indices: Vec<usize> = Vec::with_capacity(lowered_needle.len());
    let mut last_lower_pos: Option<usize> = None;
    let mut cur = 0usize;
    for &nc in &lowered_needle {
        let mut found_at: Option<usize> = None;
        while cur < lowered_chars.len() {
            if lowered_chars[cur] == nc {
                found_at = Some(cur);
                cur += 1;
                break;
            }
            cur += 1;
        }
        let pos = found_at?;
        result_orig_indices.push(lowered_to_orig_char_idx[pos]);
        last_lower_pos = Some(pos);
    }

    let first_lower_pos = if result_orig_indices.is_empty() {
        0usize
    } else {
        let target_orig = result_orig_indices[0];
        lowered_to_orig_char_idx
            .iter()
            .position(|&oi| oi == target_orig)
            .unwrap_or(0)
    };

    let last_lower_pos = last_lower_pos.unwrap_or(first_lower_pos);
    let window =
        (last_lower_pos as i32 - first_lower_pos as i32 + 1) - (lowered_needle.len() as i32);
    let mut score = window.max(0);
    if first_lower_pos == 0 {
        score -= 100;
    }

    result_orig_indices.sort_unstable();
    result_orig_indices.dedup();
    Some((result_orig_indices, score))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn fuzzy_match_prefers_prefix_matches() {
        let (_idx_a, score_a) = fuzzy_match("abc", "abc").expect("match a");
        let (_idx_b, score_b) = fuzzy_match("zabc", "abc").expect("match b");
        assert!(score_a < score_b);
    }

    #[test]
    fn popup_renders_with_hint_line() {
        let popup = SkillPopup::new(vec![MentionItem {
            display_name: "skill-creator".to_string(),
            description: Some("Create or update a skill".to_string()),
            insert_text: "$skill-creator".to_string(),
            search_terms: vec!["skill-creator".to_string()],
        }]);

        let mut terminal = Terminal::new(TestBackend::new(50, 5)).expect("terminal");
        terminal
            .draw(|f| (&popup).render_ref(f.area(), f.buffer_mut()))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }
}
