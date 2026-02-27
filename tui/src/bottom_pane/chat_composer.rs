//! The chat composer is the bottom-pane text input state machine.
//!
//! It is responsible for:
//!
//! - Editing the input buffer (a [`TextArea`]), including placeholder "elements" for large pastes.
//! - Routing keys to the active popup (file search and skills picker).
//! - Handling submit vs newline on Enter.
//! - Inserting literal tab characters on Tab (when no popup is visible).
//! - Turning raw key streams into explicit paste operations on platforms where terminals
//!   don't provide reliable bracketed paste (notably Windows).
//!
//! # Divergences from upstream Codex TUI
//!
//! See `tui/AGENTS.md` for the canonical list. Highlights:
//!
//! - No `/` command picker popup and no `?` shortcuts overlay (`/` and `?` are inserted literally).
//! - `Esc` only dismisses popups; there is no Esc-driven rewind/backtrack UX.
//! - No steer mode: <kbd>Enter</kbd> queues the message instead of submitting immediately.
//! - The skills picker is driven by `$`-mentions.
//! - No image pasting support (text-only paste).
//! - Placeholder text uses CodexPotter wording (`Assign new task to CodexPotter`) rather than
//!   upstream (`Ask Codex to do anything`).
//!
//! # Key Event Routing
//!
//! Most key handling goes through [`ChatComposer::handle_key_event`], which dispatches to a
//! popup-specific handler if a popup is visible and otherwise to
//! [`ChatComposer::handle_key_event_without_popup`]. After every handled key, we call
//! [`ChatComposer::sync_popups`] so UI state follows the latest buffer/cursor.
//!
//! # History Navigation (Up/Down)
//!
//! The composer supports shell-style prompt history recall using:
//!
//! - <kbd>↑</kbd>/<kbd>↓</kbd>, and
//! - <kbd>Ctrl</kbd>+<kbd>P</kbd>/<kbd>Ctrl</kbd>+<kbd>N</kbd>.
//!
//! To avoid hijacking normal cursor movement, these keys only trigger history navigation when:
//!
//! - The input is empty, **or**
//! - The cursor is at column 0 and the current text matches the last history-filled entry.
//!
//! After recalling an entry, the cursor is reset to column 0. If the user edits the recalled text
//! (or moves the cursor away from column 0), subsequent <kbd>↑</kbd>/<kbd>↓</kbd> revert to normal
//! cursor movement.
//!
//! In `codex-potter`, prompt history is persisted to `~/.codexpotter/history.jsonl` (last 500
//! entries).
//!
//! # Non-bracketed Paste Bursts
//!
//! On some terminals (especially on Windows), pastes arrive as a rapid sequence of
//! `KeyCode::Char` and `KeyCode::Enter` key events instead of a single paste event.
//!
//! To avoid misinterpreting these bursts as real typing (and to prevent transient UI effects like
//! accidental submissions mid-paste), we feed "plain" character events into
//! [`PasteBurst`](super::paste_burst::PasteBurst), which buffers bursts and later flushes them
//! through [`ChatComposer::handle_paste`].
//!
//! The burst detector intentionally treats ASCII and non-ASCII differently:
//!
//! - ASCII: we briefly hold the first fast char (flicker suppression) until we know whether the
//!   stream is paste-like.
//! - non-ASCII: we do not hold the first char (IME input would feel dropped), but we still allow
//!   burst detection for actual paste streams.
//!
//! The burst detector can also be disabled (`disable_paste_burst`), which bypasses the state
//! machine and treats the key stream as normal typing. When toggling from enabled → disabled, the
//! composer flushes/clears any in-flight burst state so it cannot leak into subsequent input.
//!
//! For the detailed burst state machine, see `tui/src/bottom_pane/paste_burst.rs`.
//! For a narrative overview of the combined state machine, see `docs/wiki/tui-chat-composer.md`.
//!
//! # PasteBurst Integration Points
//!
//! The burst detector is consulted in a few specific places:
//!
//! - [`ChatComposer::handle_input_basic`]: flushes any due burst first, then intercepts plain char
//!   input to either buffer it or insert normally.
//! - [`ChatComposer::handle_non_ascii_char`]: handles the non-ASCII/IME path without holding the
//!   first char, while still allowing paste detection via retro-capture.
//! - [`ChatComposer::flush_paste_burst_if_due`]/[`ChatComposer::handle_paste_burst_flush`]: called
//!   from UI ticks to turn a pending burst into either an explicit paste (`handle_paste`) or a
//!   normal typed character.
//!
//! # Input Disabled Mode
//!
//! The composer can be temporarily read-only (`input_enabled = false`). In that mode it ignores
//! edits and renders a placeholder prompt instead of the editable textarea. This is part of the
//! overall state machine, since it affects which transitions are even possible from a given UI
//! state.
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::key_hint::has_ctrl_or_alt;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Margin;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::WidgetRef;

use super::chat_composer_history::ChatComposerHistory;
use super::chat_composer_history::HistoryEntry;
use super::file_search_popup::FileSearchPopup;
use super::footer::FooterMode;
use super::footer::FooterProps;
use super::footer::footer_height;
use super::footer::render_footer;
use super::footer::reset_mode_after_activity;
use super::paste_burst::CharDecision;
use super::paste_burst::PasteBurst;
use super::skill_popup::MentionItem;
use super::skill_popup::SkillPopup;
use crate::bottom_pane::paste_burst::FlushResult;
use crate::render::Insets;
use crate::render::RectExt;
use crate::render::renderable::Renderable;
use crate::style::user_message_style;
use codex_file_search::FileMatch;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::textarea::TextArea;
use crate::bottom_pane::textarea::TextAreaState;
use crate::skills_discovery::SkillMetadata;
use crate::ui_consts::LIVE_PREFIX_COLS;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

/// If the pasted content exceeds this number of characters, replace it with a
/// placeholder in the UI.
const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;

/// Result returned when the user interacts with the text area.
///
/// # Divergence (codex-potter)
///
/// `codex-potter` does not implement "steer mode": pressing <kbd>Enter</kbd> queues the message for
/// the runner (it does not submit immediately).
#[derive(Debug, PartialEq)]
pub enum InputResult {
    Submitted(String),
    Queued(String),
    None,
}

#[derive(Clone, Debug, PartialEq)]
/// Serializable snapshot of the composer state that can be restored across turns.
pub struct ChatComposerDraft {
    text: String,
    cursor: usize,
    pending_pastes: Vec<(String, String)>,
    large_paste_counters: HashMap<usize, usize>,
}

impl ChatComposerDraft {
    fn is_empty(&self) -> bool {
        self.text.is_empty() && self.pending_pastes.is_empty()
    }
}

/// Bottom-pane chat input state machine.
pub struct ChatComposer {
    textarea: TextArea,
    textarea_state: RefCell<TextAreaState>,
    active_popup: ActivePopup,
    app_event_tx: AppEventSender,
    skills: Vec<SkillMetadata>,
    history: ChatComposerHistory,
    quit_shortcut_expires_at: Option<Instant>,
    quit_shortcut_key: KeyBinding,
    dismissed_file_popup_token: Option<String>,
    dismissed_skill_popup_token: Option<String>,
    current_file_query: Option<String>,
    pending_pastes: Vec<(String, String)>,
    large_paste_counters: HashMap<usize, usize>,
    placeholder_text: String,
    /// When false, the composer is temporarily read-only (e.g. during sandbox setup).
    input_enabled: bool,
    input_disabled_placeholder: Option<String>,
    /// Non-bracketed paste burst tracker (see `bottom_pane/paste_burst.rs`).
    paste_burst: PasteBurst,
    // When true, disables paste-burst logic and inserts characters immediately.
    disable_paste_burst: bool,
    footer_mode: FooterMode,
    footer_hint_override: Option<Vec<(String, String)>>,
    context_window_percent: Option<i64>,
    context_window_used_tokens: Option<i64>,
}

/// Popup state – at most one can be visible at any time.
enum ActivePopup {
    None,
    File(FileSearchPopup),
    Skill(SkillPopup),
}

const FOOTER_SPACING_HEIGHT: u16 = 0;

impl ChatComposer {
    pub fn new(
        _has_input_focus: bool,
        app_event_tx: AppEventSender,
        _enhanced_keys_supported: bool,
        placeholder_text: String,
        disable_paste_burst: bool,
    ) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let skills = crate::skills_discovery::load_skills(&cwd);

        let mut this = Self {
            textarea: TextArea::new(),
            textarea_state: RefCell::new(TextAreaState::default()),
            active_popup: ActivePopup::None,
            app_event_tx,
            skills,
            history: ChatComposerHistory::new(),
            quit_shortcut_expires_at: None,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            dismissed_file_popup_token: None,
            dismissed_skill_popup_token: None,
            current_file_query: None,
            pending_pastes: Vec::new(),
            large_paste_counters: HashMap::new(),
            placeholder_text,
            input_enabled: true,
            input_disabled_placeholder: None,
            paste_burst: PasteBurst::default(),
            disable_paste_burst: false,
            footer_mode: FooterMode::ShortcutSummary,
            footer_hint_override: None,
            context_window_percent: None,
            context_window_used_tokens: None,
        };
        // Apply configuration via the setter to keep side-effects centralized.
        this.set_disable_paste_burst(disable_paste_burst);
        this
    }

    fn layout_areas(&self, area: Rect) -> [Rect; 3] {
        let footer_props = self.footer_props();
        let footer_hint_height = self
            .custom_footer_height()
            .unwrap_or_else(|| footer_height(footer_props));
        let footer_spacing = Self::footer_spacing(footer_hint_height);
        let footer_total_height = footer_hint_height + footer_spacing;
        let popup_constraint = match &self.active_popup {
            ActivePopup::File(popup) => Constraint::Max(popup.calculate_required_height()),
            ActivePopup::Skill(popup) => Constraint::Max(popup.calculate_required_height()),
            ActivePopup::None => Constraint::Max(footer_total_height),
        };
        let [composer_rect, popup_rect] =
            Layout::vertical([Constraint::Min(3), popup_constraint]).areas(area);
        let textarea_rect = composer_rect.inset(Insets::tlbr(1, LIVE_PREFIX_COLS, 1, 1));
        [composer_rect, textarea_rect, popup_rect]
    }

    fn footer_spacing(footer_hint_height: u16) -> u16 {
        if footer_hint_height == 0 {
            0
        } else {
            FOOTER_SPACING_HEIGHT
        }
    }

    /// Returns true if the composer currently contains no user input.
    pub fn is_empty(&self) -> bool {
        self.textarea.is_empty()
    }

    /// Integrate pasted text into the composer.
    ///
    /// Acts as the only place where paste text is integrated, both for:
    ///
    /// - Real/explicit paste events surfaced by the terminal, and
    /// - Non-bracketed "paste bursts" that [`PasteBurst`](super::paste_burst::PasteBurst) buffers
    ///   and later flushes here.
    ///
    /// Behavior:
    ///
    /// - If the paste is larger than `LARGE_PASTE_CHAR_THRESHOLD` chars, inserts a placeholder
    ///   element (expanded on submit) and stores the full text in `pending_pastes`.
    /// - Otherwise, inserts the pasted text directly into the textarea.
    ///
    /// In all cases, clears any paste-burst Enter suppression state so a real paste cannot affect
    /// the next user Enter key, then syncs popup state.
    pub fn handle_paste(&mut self, pasted: String) -> bool {
        let char_count = pasted.chars().count();
        if char_count > LARGE_PASTE_CHAR_THRESHOLD {
            let placeholder = self.next_large_paste_placeholder(char_count);
            self.textarea.insert_element(&placeholder);
            self.pending_pastes.push((placeholder, pasted));
        } else {
            self.textarea.insert_str(&pasted);
        }
        // Explicit paste events should not trigger Enter suppression.
        self.paste_burst.clear_after_explicit_paste();
        self.sync_popups();
        true
    }

    /// Enable or disable paste-burst handling.
    ///
    /// `disable_paste_burst` is an escape hatch for terminals/platforms where the burst heuristic
    /// is unwanted or has already been handled elsewhere.
    ///
    /// When transitioning from enabled → disabled, we "defuse" any in-flight burst state so it
    /// cannot affect subsequent normal typing:
    ///
    /// - First, flush any held/buffered text immediately via
    ///   [`PasteBurst::flush_before_modified_input`], and feed it through `handle_paste(String)`.
    ///   This preserves user input and routes it through the same integration path as explicit
    ///   pastes (large-paste placeholders and popup sync).
    /// - Then clear the burst timing and Enter-suppression window via
    ///   [`PasteBurst::clear_after_explicit_paste`].
    ///
    /// We intentionally do not use `clear_window_after_non_char()` here: it clears timing state
    /// without emitting any buffered text, which can leave a non-empty buffer unable to flush
    /// later (because `flush_if_due()` relies on `last_plain_char_time` to time out).
    pub fn set_disable_paste_burst(&mut self, disabled: bool) {
        let was_disabled = self.disable_paste_burst;
        self.disable_paste_burst = disabled;
        if disabled && !was_disabled {
            if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                self.handle_paste(pasted);
            }
            self.paste_burst.clear_after_explicit_paste();
        }
    }

    /// Replace the composer content with text from an external editor.
    ///
    /// This clears any pending large-paste placeholders, since the returned text is now the
    /// single source of truth.
    pub fn apply_external_edit(&mut self, text: String) {
        self.pending_pastes.clear();
        self.large_paste_counters.clear();

        self.textarea.set_text_clearing_elements(&text);
        self.textarea.set_cursor(text.len());
        self.sync_popups();
    }

    pub fn take_draft(&mut self) -> Option<ChatComposerDraft> {
        // Avoid dropping any buffered paste-burst input on suspend.
        if !self.disable_paste_burst {
            if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                self.handle_paste(pasted);
            }
            self.paste_burst.clear_after_explicit_paste();
        }

        let draft = ChatComposerDraft {
            text: self.textarea.text().to_string(),
            cursor: self.textarea.cursor(),
            pending_pastes: self.pending_pastes.clone(),
            large_paste_counters: self.large_paste_counters.clone(),
        };

        if draft.is_empty() { None } else { Some(draft) }
    }

    pub fn restore_draft(&mut self, draft: ChatComposerDraft) {
        self.active_popup = ActivePopup::None;
        self.dismissed_file_popup_token = None;
        self.current_file_query = None;
        self.quit_shortcut_expires_at = None;

        let text = draft.text;
        self.pending_pastes = draft.pending_pastes;
        self.pending_pastes
            .retain(|(placeholder, _)| text.contains(placeholder));
        self.large_paste_counters = draft.large_paste_counters;

        // Rebuild textarea so placeholder labels become elements again.
        self.textarea.set_text_clearing_elements("");
        *self.textarea_state.borrow_mut() = TextAreaState::default();
        self.paste_burst = PasteBurst::default();

        let mut placeholder_set: HashSet<&str> = HashSet::new();
        for (placeholder, _) in &self.pending_pastes {
            placeholder_set.insert(placeholder);
        }

        let mut placeholders: Vec<&str> = placeholder_set.into_iter().collect();
        placeholders.sort_unstable_by_key(|placeholder| std::cmp::Reverse(placeholder.len()));

        let mut occurrences: Vec<(usize, &str)> = Vec::new();
        for placeholder in &placeholders {
            for (pos, _) in text.match_indices(placeholder) {
                occurrences.push((pos, *placeholder));
            }
        }
        occurrences.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.len().cmp(&a.1.len())));

        let mut idx = 0usize;
        for (pos, ph) in occurrences {
            if pos < idx {
                continue;
            }
            if pos > idx {
                self.textarea.insert_str(&text[idx..pos]);
            }
            self.textarea.insert_element(ph);
            idx = pos + ph.len();
        }
        if idx < text.len() {
            self.textarea.insert_str(&text[idx..]);
        }

        let new_cursor = Self::clamp_to_char_boundary(self.textarea.text(), draft.cursor);
        self.textarea.set_cursor(new_cursor);
        self.sync_popups();
    }

    pub fn current_text_with_pending(&self) -> String {
        let mut text = self.textarea.text().to_string();
        for (placeholder, actual) in &self.pending_pastes {
            if text.contains(placeholder) {
                text = text.replace(placeholder, actual);
            }
        }
        text
    }

    /// Override the footer hint items displayed beneath the composer. Passing
    /// `None` restores the default shortcut footer.
    pub fn set_footer_hint_override(&mut self, items: Option<Vec<(String, String)>>) {
        self.footer_hint_override = items;
    }

    pub fn set_history_metadata(&mut self, log_id: u64, entry_count: usize) {
        self.history.set_metadata(log_id, entry_count);
    }

    pub fn on_history_entry_response(
        &mut self,
        log_id: u64,
        offset: usize,
        entry: Option<String>,
    ) -> bool {
        let Some(entry) = self.history.on_entry_response(log_id, offset, entry) else {
            return false;
        };
        self.set_text_content(entry.text);
        true
    }

    /// Replace the entire composer content with `text` and reset cursor.
    pub fn set_text_content(&mut self, text: String) {
        // Clear any existing content and placeholders first.
        self.textarea.set_text_clearing_elements("");
        self.pending_pastes.clear();
        self.textarea.set_text_clearing_elements(&text);
        self.textarea.set_cursor(0);
        self.sync_popups();
    }

    pub fn clear_for_ctrl_c(&mut self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let previous = self.textarea.text().to_string();
        self.set_text_content(String::new());
        self.history.reset_navigation();
        self.history
            .record_local_submission(HistoryEntry::from_text(previous.clone()));
        Some(previous)
    }

    /// Get the current composer text.
    #[cfg(test)]
    pub fn current_text(&self) -> String {
        self.textarea.text().to_string()
    }

    /// Flushes any due paste-burst state.
    ///
    /// Call this from a UI tick to turn paste-burst transient state into explicit textarea edits:
    ///
    /// - If a burst times out, flush it via `handle_paste(String)`.
    /// - If only the first ASCII char was held (flicker suppression) and no burst followed, emit it
    ///   as normal typed input.
    ///
    /// This also allows a single "held" ASCII char to render even when it turns out not to be part
    /// of a paste burst.
    pub fn flush_paste_burst_if_due(&mut self) -> bool {
        self.handle_paste_burst_flush(Instant::now())
    }

    /// Returns whether the composer is currently in any paste-burst related transient state.
    ///
    /// This includes actively buffering, having a non-empty burst buffer, or holding the first
    /// ASCII char for flicker suppression.
    pub fn is_in_paste_burst(&self) -> bool {
        self.paste_burst.is_active()
    }

    /// Returns a delay that reliably exceeds the paste-burst timing threshold.
    ///
    /// Use this in tests to avoid boundary flakiness around the `PasteBurst` timeout.
    pub fn recommended_paste_flush_delay() -> Duration {
        PasteBurst::recommended_flush_delay()
    }

    /// Integrate results from an asynchronous file search.
    pub fn on_file_search_result(&mut self, query: String, matches: Vec<FileMatch>) {
        // Only apply if user is still editing a token starting with `query`.
        let current_opt = Self::current_at_token(&self.textarea);
        let Some(current_token) = current_opt else {
            return;
        };

        if !current_token.starts_with(&query) {
            return;
        }

        if let ActivePopup::File(popup) = &mut self.active_popup {
            popup.set_matches(&query, matches);
        }
    }

    /// Show the transient "press again to quit" hint for `key`.
    ///
    /// The owner (`BottomPane`/`ChatWidget`) is responsible for scheduling a
    /// redraw after [`super::QUIT_SHORTCUT_TIMEOUT`] so the hint can disappear
    /// even when the UI is otherwise idle.
    #[cfg(test)]
    pub fn show_quit_shortcut_hint(&mut self, key: KeyBinding, _has_focus: bool) {
        self.quit_shortcut_expires_at = Instant::now()
            .checked_add(super::QUIT_SHORTCUT_TIMEOUT)
            .or_else(|| Some(Instant::now()));
        self.quit_shortcut_key = key;
        self.footer_mode = FooterMode::QuitShortcutReminder;
    }

    /// Whether the quit shortcut hint should currently be shown.
    ///
    /// This is time-based rather than event-based: it may become false without
    /// any additional user input, so the UI schedules a redraw when the hint
    /// expires.
    pub fn quit_shortcut_hint_visible(&self) -> bool {
        self.quit_shortcut_expires_at
            .is_some_and(|expires_at| Instant::now() < expires_at)
    }

    fn next_large_paste_placeholder(&mut self, char_count: usize) -> String {
        let base = format!("[Pasted Content {char_count} chars]");
        let next_suffix = self.large_paste_counters.entry(char_count).or_insert(0);
        *next_suffix += 1;
        if *next_suffix == 1 {
            base
        } else {
            format!("{base} #{next_suffix}")
        }
    }

    /// Handle a key event coming from the main UI.
    pub fn handle_key_event(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if !self.input_enabled {
            return (InputResult::None, false);
        }

        let result = match &mut self.active_popup {
            ActivePopup::File(_) => self.handle_key_event_with_file_popup(key_event),
            ActivePopup::Skill(_) => self.handle_key_event_with_skill_popup(key_event),
            ActivePopup::None => self.handle_key_event_without_popup(key_event),
        };

        // Update (or hide/show) popup after processing the key.
        self.sync_popups();

        result
    }

    #[inline]
    fn clamp_to_char_boundary(text: &str, pos: usize) -> usize {
        let mut p = pos.min(text.len());
        if p < text.len() && !text.is_char_boundary(p) {
            p = text
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= p)
                .last()
                .unwrap_or(0);
        }
        p
    }

    /// Handle non-ASCII character input (often IME) while still supporting paste-burst detection.
    ///
    /// This handler exists because non-ASCII input often comes from IMEs, where characters can
    /// legitimately arrive in short bursts that should **not** be treated as paste.
    ///
    /// The key differences from the ASCII path:
    ///
    /// - We never hold the first character (`PasteBurst::on_plain_char_no_hold`), because holding a
    ///   non-ASCII char can feel like dropped input.
    /// - If a burst is detected, we may need to retroactively remove already-inserted text before
    ///   the cursor and move it into the paste buffer (see `PasteBurst::decide_begin_buffer`).
    ///
    /// Because this path mixes "insert immediately" with "maybe retro-grab later", it must clamp
    /// the cursor to a UTF-8 char boundary before slicing `textarea.text()`.
    #[inline]
    fn handle_non_ascii_char(&mut self, input: KeyEvent) -> (InputResult, bool) {
        if self.disable_paste_burst {
            // When burst detection is disabled, treat IME/non-ASCII input as normal typing.
            // In particular, do not retro-capture or buffer already-inserted prefix text.
            self.textarea.input(input);
            let text_after = self.textarea.text();
            self.pending_pastes
                .retain(|(placeholder, _)| text_after.contains(placeholder));
            return (InputResult::None, true);
        }
        if let KeyEvent {
            code: KeyCode::Char(ch),
            ..
        } = input
        {
            let now = Instant::now();
            if self.paste_burst.try_append_char_if_active(ch, now) {
                return (InputResult::None, true);
            }
            // Non-ASCII input often comes from IMEs and can arrive in quick bursts.
            // We do not want to hold the first char (flicker suppression) on this path, but we
            // still want to detect paste-like bursts. Before applying any non-ASCII input, flush
            // any existing burst buffer (including a pending first char from the ASCII path) so
            // we don't carry that transient state forward.
            if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                self.handle_paste(pasted);
            }
            if let Some(decision) = self.paste_burst.on_plain_char_no_hold(now) {
                match decision {
                    CharDecision::BufferAppend => {
                        self.paste_burst.append_char_to_buffer(ch, now);
                        return (InputResult::None, true);
                    }
                    CharDecision::BeginBuffer { retro_chars } => {
                        // For non-ASCII we inserted prior chars immediately, so if this turns out
                        // to be paste-like we need to retroactively grab & remove the already-
                        // inserted prefix from the textarea before buffering the burst.
                        let cur = self.textarea.cursor();
                        let txt = self.textarea.text();
                        let safe_cur = Self::clamp_to_char_boundary(txt, cur);
                        let before = &txt[..safe_cur];
                        if let Some(grab) =
                            self.paste_burst
                                .decide_begin_buffer(now, before, retro_chars as usize)
                        {
                            if !grab.grabbed.is_empty() {
                                self.textarea.replace_range(grab.start_byte..safe_cur, "");
                            }
                            // seed the paste burst buffer with everything (grabbed + new)
                            self.paste_burst.append_char_to_buffer(ch, now);
                            return (InputResult::None, true);
                        }
                        // If decide_begin_buffer opted not to start buffering,
                        // fall through to normal insertion below.
                    }
                    _ => unreachable!("on_plain_char_no_hold returned unexpected variant"),
                }
            }
        }
        if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
            self.handle_paste(pasted);
        }
        self.textarea.input(input);
        let text_after = self.textarea.text();
        self.pending_pastes
            .retain(|(placeholder, _)| text_after.contains(placeholder));
        (InputResult::None, true)
    }

    /// Handle key events when file search popup is visible.
    fn handle_key_event_with_file_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        self.footer_mode = reset_mode_after_activity(self.footer_mode);
        let ActivePopup::File(popup) = &mut self.active_popup else {
            unreachable!();
        };

        match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                // Hide popup without modifying text, remember token to avoid immediate reopen.
                if let Some(tok) = Self::current_at_token(&self.textarea) {
                    self.dismissed_file_popup_token = Some(tok);
                }
                self.active_popup = ActivePopup::None;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                let Some(sel) = popup.selected_match() else {
                    self.active_popup = ActivePopup::None;
                    return (InputResult::None, true);
                };

                let sel_path = sel.to_string();
                self.insert_selected_path(&sel_path);
                // No selection: treat Enter as closing the popup/session.
                self.active_popup = ActivePopup::None;
                (InputResult::None, true)
            }
            input => self.handle_input_basic(input),
        }
    }

    fn handle_key_event_with_skill_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        self.footer_mode = reset_mode_after_activity(self.footer_mode);

        let ActivePopup::Skill(popup) = &mut self.active_popup else {
            unreachable!();
        };

        let mut selected_mention: Option<String> = None;
        let mut close_popup = false;

        let result = match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                // Hide popup without modifying text, remember token to avoid immediate reopen.
                if let Some(tok) = self.current_mention_token() {
                    self.dismissed_skill_popup_token = Some(tok);
                }
                self.active_popup = ActivePopup::None;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if let Some(mention) = popup.selected_mention() {
                    selected_mention = Some(mention.insert_text.clone());
                }
                close_popup = true;
                (InputResult::None, true)
            }
            input => self.handle_input_basic(input),
        };

        if close_popup {
            if let Some(insert_text) = selected_mention {
                self.insert_selected_mention(&insert_text);
            }
            self.active_popup = ActivePopup::None;
        }

        result
    }

    /// Extract a token prefixed with `prefix` under the cursor, if any.
    ///
    /// The returned string **does not** include the prefix.
    ///
    /// Behavior:
    /// - The cursor may be anywhere *inside* the token (including on the
    ///   leading prefix). It does **not** need to be at the end of the line.
    /// - A token is delimited by ASCII whitespace (space, tab, newline).
    /// - If the token under the cursor starts with `prefix`, that token is
    ///   returned without the leading prefix. When `allow_empty` is true, a
    ///   lone prefix character yields `Some(String::new())` to surface hints.
    fn current_prefixed_token(
        textarea: &TextArea,
        prefix: char,
        allow_empty: bool,
    ) -> Option<String> {
        let cursor_offset = textarea.cursor();
        let text = textarea.text();

        // Adjust the provided byte offset to the nearest valid char boundary at or before it.
        let mut safe_cursor = cursor_offset.min(text.len());
        // If we're not on a char boundary, move back to the start of the current char.
        if safe_cursor < text.len() && !text.is_char_boundary(safe_cursor) {
            // Find the last valid boundary <= cursor_offset.
            safe_cursor = text
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= cursor_offset)
                .last()
                .unwrap_or(0);
        }

        // Split the line around the (now safe) cursor position.
        let before_cursor = &text[..safe_cursor];
        let after_cursor = &text[safe_cursor..];

        // Detect whether we're on whitespace at the cursor boundary.
        let at_whitespace = if safe_cursor < text.len() {
            text[safe_cursor..]
                .chars()
                .next()
                .map(char::is_whitespace)
                .unwrap_or(false)
        } else {
            false
        };

        // Left candidate: token containing the cursor position.
        let start_left = before_cursor
            .char_indices()
            .rfind(|(_, c)| c.is_whitespace())
            .map(|(idx, c)| idx + c.len_utf8())
            .unwrap_or(0);
        let end_left_rel = after_cursor
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or(after_cursor.len());
        let end_left = safe_cursor + end_left_rel;
        let token_left = if start_left < end_left {
            Some(&text[start_left..end_left])
        } else {
            None
        };

        // Right candidate: token immediately after any whitespace from the cursor.
        let ws_len_right: usize = after_cursor
            .chars()
            .take_while(|c| c.is_whitespace())
            .map(char::len_utf8)
            .sum();
        let start_right = safe_cursor + ws_len_right;
        let end_right_rel = text[start_right..]
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or(text.len() - start_right);
        let end_right = start_right + end_right_rel;
        let token_right = if start_right < end_right {
            Some(&text[start_right..end_right])
        } else {
            None
        };

        let prefix_str = prefix.to_string();
        let left_match = token_left.filter(|t| t.starts_with(prefix));
        let right_match = token_right.filter(|t| t.starts_with(prefix));

        let left_prefixed = left_match.map(|t| t[prefix.len_utf8()..].to_string());
        let right_prefixed = right_match.map(|t| t[prefix.len_utf8()..].to_string());

        if at_whitespace {
            if right_prefixed.is_some() {
                return right_prefixed;
            }
            if token_left.is_some_and(|t| t == prefix_str) {
                return allow_empty.then(String::new);
            }
            return left_prefixed;
        }
        if after_cursor.starts_with(prefix) {
            return right_prefixed.or(left_prefixed);
        }
        left_prefixed.or(right_prefixed)
    }

    /// Extract the `@token` that the cursor is currently positioned on, if any.
    ///
    /// The returned string **does not** include the leading `@`.
    fn current_at_token(textarea: &TextArea) -> Option<String> {
        Self::current_prefixed_token(textarea, '@', false)
    }

    fn current_mention_token(&self) -> Option<String> {
        Self::current_prefixed_token(&self.textarea, '$', true)
    }

    /// Replace the active `@token` (the one under the cursor) with `path`.
    ///
    /// The algorithm mirrors `current_at_token` so replacement works no matter
    /// where the cursor is within the token and regardless of how many
    /// `@tokens` exist in the line.
    fn insert_selected_path(&mut self, path: &str) {
        let cursor_offset = self.textarea.cursor();
        let text = self.textarea.text();
        // Clamp to a valid char boundary to avoid panics when slicing.
        let safe_cursor = Self::clamp_to_char_boundary(text, cursor_offset);

        let before_cursor = &text[..safe_cursor];
        let after_cursor = &text[safe_cursor..];

        // Determine token boundaries.
        let start_idx = before_cursor
            .char_indices()
            .rfind(|(_, c)| c.is_whitespace())
            .map(|(idx, c)| idx + c.len_utf8())
            .unwrap_or(0);

        let end_rel_idx = after_cursor
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or(after_cursor.len());
        let end_idx = safe_cursor + end_rel_idx;

        // If the path contains whitespace, wrap it in double quotes so the
        // local prompt arg parser treats it as a single argument. Avoid adding
        // quotes when the path already contains one to keep behavior simple.
        let needs_quotes = path.chars().any(char::is_whitespace);
        let inserted = if needs_quotes && !path.contains('"') {
            format!("\"{path}\"")
        } else {
            path.to_string()
        };

        // Replace the slice `[start_idx, end_idx)` with the chosen path and a trailing space.
        let mut new_text =
            String::with_capacity(text.len() - (end_idx - start_idx) + inserted.len() + 1);
        new_text.push_str(&text[..start_idx]);
        new_text.push_str(&inserted);
        new_text.push(' ');
        new_text.push_str(&text[end_idx..]);

        self.textarea.set_text_clearing_elements(&new_text);
        let new_cursor = start_idx.saturating_add(inserted.len()).saturating_add(1);
        self.textarea.set_cursor(new_cursor);
    }

    fn insert_selected_mention(&mut self, insert_text: &str) {
        let cursor_offset = self.textarea.cursor();
        let text = self.textarea.text();
        let safe_cursor = Self::clamp_to_char_boundary(text, cursor_offset);

        let before_cursor = &text[..safe_cursor];
        let after_cursor = &text[safe_cursor..];

        let start_idx = before_cursor
            .char_indices()
            .rfind(|(_, c)| c.is_whitespace())
            .map(|(idx, c)| idx + c.len_utf8())
            .unwrap_or(0);

        let end_rel_idx = after_cursor
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or(after_cursor.len());
        let end_idx = safe_cursor + end_rel_idx;

        let inserted = insert_text.to_string();

        let mut new_text =
            String::with_capacity(text.len() - (end_idx - start_idx) + inserted.len() + 1);
        new_text.push_str(&text[..start_idx]);
        new_text.push_str(&inserted);
        new_text.push(' ');
        new_text.push_str(&text[end_idx..]);

        self.textarea.set_text_clearing_elements(&new_text);
        let new_cursor = start_idx.saturating_add(inserted.len()).saturating_add(1);
        self.textarea.set_cursor(new_cursor);
    }

    /// Prepare text for submission/queuing. Returns None if submission should be suppressed.
    fn prepare_submission_text(&mut self) -> Option<String> {
        let mut text = self.textarea.text().to_string();
        self.textarea.set_text_clearing_elements("");

        // Replace any placeholder pastes in the text before submission.
        if !self.pending_pastes.is_empty() {
            for (placeholder, actual) in &self.pending_pastes {
                if text.contains(placeholder) {
                    text = text.replace(placeholder, actual);
                }
            }
            self.pending_pastes.clear();
        }

        text = text.trim().to_string();

        if text.is_empty() {
            return None;
        }
        self.history
            .record_local_submission(HistoryEntry::from_text(text.clone()));
        Some(text)
    }

    /// Common logic for handling message submission/queuing.
    /// Returns the appropriate InputResult based on `should_queue`.
    fn handle_submission(&mut self, should_queue: bool) -> (InputResult, bool) {
        // If we're in a paste-like burst capture, treat Enter/Ctrl+Shift+Q as part of the burst
        // and accumulate it rather than submitting or inserting immediately.
        if !self.disable_paste_burst && self.paste_burst.is_active() {
            let now = Instant::now();
            if self.paste_burst.append_newline_if_active(now) {
                return (InputResult::None, true);
            }
        }

        // During a paste-like burst, treat Enter/Ctrl+Shift+Q as a newline instead of submit.
        let now = Instant::now();
        if !self.disable_paste_burst
            && self
                .paste_burst
                .newline_should_insert_instead_of_submit(now)
        {
            self.textarea.insert_str("\n");
            self.paste_burst.extend_window(now);
            return (InputResult::None, true);
        }

        let original_input = self.textarea.text().to_string();
        let original_text_elements = self.textarea.text_elements();
        let original_cursor = self.textarea.cursor();
        let original_pending_pastes = self.pending_pastes.clone();

        if let Some(text) = self.prepare_submission_text() {
            if should_queue {
                (InputResult::Queued(text), true)
            } else {
                (InputResult::Submitted(text), true)
            }
        } else {
            // Restore text if submission was suppressed.
            self.textarea
                .set_text_with_elements(&original_input, &original_text_elements);
            self.textarea.set_cursor(original_cursor);
            self.pending_pastes = original_pending_pastes;
            (InputResult::None, true)
        }
    }

    /// Handle key event when no popup is visible.
    fn handle_key_event_without_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        self.footer_mode = reset_mode_after_activity(self.footer_mode);
        match key_event {
            KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } if self.is_empty() => (InputResult::None, false),
            // -------------------------------------------------------------
            // History navigation (Up / Down) – only when the composer is not
            // empty or when the cursor is at the correct position, to avoid
            // interfering with normal cursor movement.
            // -------------------------------------------------------------
            KeyEvent {
                code: KeyCode::Up | KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('p') | KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                if self
                    .history
                    .should_handle_navigation(self.textarea.text(), self.textarea.cursor())
                {
                    let replace_entry = match key_event.code {
                        KeyCode::Up => self.history.navigate_up(&self.app_event_tx),
                        KeyCode::Down => self.history.navigate_down(&self.app_event_tx),
                        KeyCode::Char('p') => self.history.navigate_up(&self.app_event_tx),
                        KeyCode::Char('n') => self.history.navigate_down(&self.app_event_tx),
                        _ => unreachable!(),
                    };
                    if let Some(entry) = replace_entry {
                        self.set_text_content(entry.text);
                    }
                    return (InputResult::None, true);
                }
                self.handle_input_basic(key_event)
            }
            tab_event @ KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                ..
            } => self.handle_input_basic(KeyEvent {
                code: KeyCode::Char('\t'),
                ..tab_event
            }),
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => self.handle_submission(true),
            input => self.handle_input_basic(input),
        }
    }

    /// Applies any due `PasteBurst` flush at time `now`.
    ///
    /// Converts [`PasteBurst::flush_if_due`] results into concrete textarea mutations.
    ///
    /// Callers:
    ///
    /// - UI ticks via [`ChatComposer::flush_paste_burst_if_due`], so held first-chars can render.
    /// - Input handling via [`ChatComposer::handle_input_basic`], so a due burst does not lag.
    fn handle_paste_burst_flush(&mut self, now: Instant) -> bool {
        match self.paste_burst.flush_if_due(now) {
            FlushResult::Paste(pasted) => {
                self.handle_paste(pasted);
                true
            }
            FlushResult::Typed(ch) => {
                // Mirror insert_str() behavior so popups stay in sync when a
                // pending fast char flushes as normal typed input.
                self.textarea.insert_str(ch.to_string().as_str());
                self.sync_popups();
                true
            }
            FlushResult::None => false,
        }
    }

    /// Handles keys that mutate the textarea, including paste-burst detection.
    ///
    /// Acts as the lowest-level keypath for keys that mutate the textarea. It is also where plain
    /// character streams are converted into explicit paste operations on terminals that do not
    /// reliably provide bracketed paste.
    ///
    /// Ordering is important:
    ///
    /// - Always flush any *due* paste burst first so buffered text does not lag behind unrelated
    ///   edits.
    /// - Then handle the incoming key, intercepting only "plain" (no Ctrl/Alt) char input.
    /// - For non-plain keys, flush via `flush_before_modified_input()` before applying the key;
    ///   otherwise `clear_window_after_non_char()` can leave buffered text waiting without a
    ///   timestamp to time out against.
    fn handle_input_basic(&mut self, input: KeyEvent) -> (InputResult, bool) {
        // If we have a buffered non-bracketed paste burst and enough time has
        // elapsed since the last char, flush it before handling a new input.
        let now = Instant::now();
        self.handle_paste_burst_flush(now);

        self.footer_mode = reset_mode_after_activity(self.footer_mode);

        // If we're capturing a burst and receive Enter, accumulate it instead of inserting.
        if matches!(input.code, KeyCode::Enter)
            && !self.disable_paste_burst
            && self.paste_burst.is_active()
            && self.paste_burst.append_newline_if_active(now)
        {
            return (InputResult::None, true);
        }

        // Intercept plain Char inputs to optionally accumulate into a burst buffer.
        //
        // This is intentionally limited to "plain" (no Ctrl/Alt) chars so shortcuts keep their
        // normal semantics, and so we can aggressively flush/clear any burst state when non-char
        // keys are pressed.
        if let KeyEvent {
            code: KeyCode::Char(ch),
            modifiers,
            ..
        } = input
        {
            let has_ctrl_or_alt = has_ctrl_or_alt(modifiers);
            if !has_ctrl_or_alt && !self.disable_paste_burst {
                // Non-ASCII characters (e.g., from IMEs) can arrive in quick bursts, so avoid
                // holding the first char while still allowing burst detection for paste input.
                if !ch.is_ascii() {
                    return self.handle_non_ascii_char(input);
                }

                match self.paste_burst.on_plain_char(ch, now) {
                    CharDecision::BufferAppend => {
                        self.paste_burst.append_char_to_buffer(ch, now);
                        return (InputResult::None, true);
                    }
                    CharDecision::BeginBuffer { retro_chars } => {
                        let cur = self.textarea.cursor();
                        let txt = self.textarea.text();
                        let safe_cur = Self::clamp_to_char_boundary(txt, cur);
                        let before = &txt[..safe_cur];
                        if let Some(grab) =
                            self.paste_burst
                                .decide_begin_buffer(now, before, retro_chars as usize)
                        {
                            if !grab.grabbed.is_empty() {
                                self.textarea.replace_range(grab.start_byte..safe_cur, "");
                            }
                            self.paste_burst.append_char_to_buffer(ch, now);
                            return (InputResult::None, true);
                        }
                        // If decide_begin_buffer opted not to start buffering,
                        // fall through to normal insertion below.
                    }
                    CharDecision::BeginBufferFromPending => {
                        // First char was held; now append the current one.
                        self.paste_burst.append_char_to_buffer(ch, now);
                        return (InputResult::None, true);
                    }
                    CharDecision::RetainFirstChar => {
                        // Keep the first fast char pending momentarily.
                        return (InputResult::None, true);
                    }
                }
            }
            if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                self.handle_paste(pasted);
            }
        }

        // Flush any buffered burst before applying a non-char input (arrow keys, etc).
        //
        // `clear_window_after_non_char()` clears `last_plain_char_time`. If we cleared that while
        // `PasteBurst.buffer` is non-empty, `flush_if_due()` would no longer have a timestamp to
        // time out against, and the buffered paste could remain stuck until another plain char
        // arrives.
        if !matches!(input.code, KeyCode::Char(_) | KeyCode::Enter)
            && let Some(pasted) = self.paste_burst.flush_before_modified_input()
        {
            self.handle_paste(pasted);
        }
        // For non-char inputs (or after flushing), handle normally.
        // Track element removals so we can drop any corresponding placeholders without scanning
        // the full text. (Placeholders are atomic elements; when deleted, the element disappears.)
        let elements_before = if self.pending_pastes.is_empty() {
            None
        } else {
            Some(self.textarea.element_payloads())
        };

        self.textarea.input(input);

        if let Some(elements_before) = elements_before {
            self.reconcile_deleted_elements(elements_before);
        }

        // Update paste-burst heuristic for plain Char (no Ctrl/Alt) events.
        let crossterm::event::KeyEvent {
            code, modifiers, ..
        } = input;
        match code {
            KeyCode::Char(_) => {
                let has_ctrl_or_alt = has_ctrl_or_alt(modifiers);
                if has_ctrl_or_alt {
                    self.paste_burst.clear_window_after_non_char();
                }
            }
            KeyCode::Enter => {
                // Keep burst window alive (supports blank lines in paste).
            }
            _ => {
                // Other keys: clear burst window (buffer should have been flushed above if needed).
                self.paste_burst.clear_window_after_non_char();
            }
        }

        (InputResult::None, true)
    }

    fn reconcile_deleted_elements(&mut self, elements_before: Vec<String>) {
        let elements_after: HashSet<String> =
            self.textarea.element_payloads().into_iter().collect();

        for removed in elements_before
            .into_iter()
            .filter(|payload| !elements_after.contains(payload))
        {
            self.pending_pastes.retain(|(ph, _)| ph != &removed);
        }
    }

    fn footer_props(&self) -> FooterProps {
        FooterProps {
            mode: self.footer_mode(),
            quit_shortcut_key: self.quit_shortcut_key,
            context_window_percent: self.context_window_percent,
            context_window_used_tokens: self.context_window_used_tokens,
        }
    }

    fn footer_mode(&self) -> FooterMode {
        match self.footer_mode {
            FooterMode::QuitShortcutReminder if self.quit_shortcut_hint_visible() => {
                FooterMode::QuitShortcutReminder
            }
            FooterMode::QuitShortcutReminder => FooterMode::ShortcutSummary,
            FooterMode::ShortcutSummary if self.quit_shortcut_hint_visible() => {
                FooterMode::QuitShortcutReminder
            }
            FooterMode::ShortcutSummary if !self.is_empty() => FooterMode::ContextOnly,
            other => other,
        }
    }

    fn custom_footer_height(&self) -> Option<u16> {
        self.footer_hint_override
            .as_ref()
            .map(|items| if items.is_empty() { 0 } else { 1 })
    }

    fn sync_popups(&mut self) {
        let file_token = Self::current_at_token(&self.textarea);
        let browsing_history = self
            .history
            .should_handle_navigation(self.textarea.text(), self.textarea.cursor());
        // When browsing input history (shell-style Up/Down recall), skip all popup
        // synchronization so nothing steals focus from continued history navigation.
        if browsing_history {
            self.active_popup = ActivePopup::None;
            return;
        }

        let mention_token = self.current_mention_token();
        if let Some(token) = mention_token {
            self.sync_skill_popup(token);
            return;
        }
        self.dismissed_skill_popup_token = None;

        if let Some(token) = file_token {
            self.sync_file_search_popup(token);
            return;
        }

        self.dismissed_file_popup_token = None;
        if matches!(
            self.active_popup,
            ActivePopup::File(_) | ActivePopup::Skill(_)
        ) {
            self.active_popup = ActivePopup::None;
        }
    }

    fn sync_skill_popup(&mut self, query: String) {
        if self.dismissed_skill_popup_token.as_ref() == Some(&query) {
            return;
        }

        let mentions = self.mention_items();
        if mentions.is_empty() {
            self.active_popup = ActivePopup::None;
            return;
        }

        match &mut self.active_popup {
            ActivePopup::Skill(popup) => {
                popup.set_query(&query);
                popup.set_mentions(mentions);
            }
            _ => {
                let mut popup = SkillPopup::new(mentions);
                popup.set_query(&query);
                self.active_popup = ActivePopup::Skill(popup);
            }
        }
    }

    fn mention_items(&self) -> Vec<MentionItem> {
        let mut mentions = Vec::new();

        for skill in &self.skills {
            let display_name = skill.display_name().to_string();
            let description = Some(skill.display_description().to_string());
            let skill_name = skill.name.clone();
            let search_terms = if display_name == skill.name {
                vec![skill_name.clone()]
            } else {
                vec![skill_name.clone(), display_name.clone()]
            };

            mentions.push(MentionItem {
                display_name,
                description,
                insert_text: format!("${skill_name}"),
                search_terms,
            });
        }

        mentions
    }

    /// Synchronize file search popup state with the current text in the textarea.
    fn sync_file_search_popup(&mut self, query: String) {
        // If user dismissed popup for this exact query, don't reopen until text changes.
        if self.dismissed_file_popup_token.as_ref() == Some(&query) {
            return;
        }

        if !query.is_empty() {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(query.clone()));
        }

        match &mut self.active_popup {
            ActivePopup::File(popup) => {
                if query.is_empty() {
                    popup.set_empty_prompt();
                } else {
                    popup.set_query(&query);
                }
            }
            _ => {
                let mut popup = FileSearchPopup::new();
                if query.is_empty() {
                    popup.set_empty_prompt();
                } else {
                    popup.set_query(&query);
                }
                self.active_popup = ActivePopup::File(popup);
            }
        }

        self.current_file_query = Some(query);
        self.dismissed_file_popup_token = None;
    }

    #[allow(dead_code)]
    pub fn set_input_enabled(&mut self, enabled: bool, placeholder: Option<String>) {
        self.input_enabled = enabled;
        self.input_disabled_placeholder = if enabled { None } else { placeholder };

        // Avoid leaving interactive popups open while input is blocked.
        if !enabled && !matches!(self.active_popup, ActivePopup::None) {
            self.active_popup = ActivePopup::None;
        }
    }
}

impl Renderable for ChatComposer {
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if !self.input_enabled {
            return None;
        }

        let [_, textarea_rect, _] = self.layout_areas(area);
        let state = *self.textarea_state.borrow();
        self.textarea.cursor_pos_with_state(textarea_rect, state)
    }

    fn desired_height(&self, width: u16) -> u16 {
        let footer_props = self.footer_props();
        let footer_hint_height = self
            .custom_footer_height()
            .unwrap_or_else(|| footer_height(footer_props));
        let footer_spacing = Self::footer_spacing(footer_hint_height);
        let footer_total_height = footer_hint_height + footer_spacing;
        const COLS_WITH_MARGIN: u16 = LIVE_PREFIX_COLS + 1;
        self.textarea
            .desired_height(width.saturating_sub(COLS_WITH_MARGIN))
            + 2
            + match &self.active_popup {
                ActivePopup::None => footer_total_height,
                ActivePopup::File(c) => c.calculate_required_height(),
                ActivePopup::Skill(c) => c.calculate_required_height(),
            }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let [composer_rect, textarea_rect, popup_rect] = self.layout_areas(area);
        match &self.active_popup {
            ActivePopup::File(popup) => {
                popup.render_ref(popup_rect, buf);
            }
            ActivePopup::Skill(popup) => {
                popup.render_ref(popup_rect, buf);
            }
            ActivePopup::None => {
                let footer_props = self.footer_props();
                let custom_height = self.custom_footer_height();
                let footer_hint_height =
                    custom_height.unwrap_or_else(|| footer_height(footer_props));
                let footer_spacing = Self::footer_spacing(footer_hint_height);
                let hint_rect = if footer_spacing > 0 && footer_hint_height > 0 {
                    let [_, hint_rect] = Layout::vertical([
                        Constraint::Length(footer_spacing),
                        Constraint::Length(footer_hint_height),
                    ])
                    .areas(popup_rect);
                    hint_rect
                } else {
                    popup_rect
                };
                if let Some(items) = self.footer_hint_override.as_ref() {
                    if !items.is_empty() {
                        let mut spans = Vec::with_capacity(items.len() * 4);
                        for (idx, (key, label)) in items.iter().enumerate() {
                            spans.push(" ".into());
                            spans.push(Span::styled(key.clone(), Style::default().bold()));
                            spans.push(format!(" {label}").into());
                            if idx + 1 != items.len() {
                                spans.push("   ".into());
                            }
                        }
                        let mut custom_rect = hint_rect;
                        if custom_rect.width > 2 {
                            custom_rect.x += 2;
                            custom_rect.width = custom_rect.width.saturating_sub(2);
                        }
                        Line::from(spans).render_ref(custom_rect, buf);
                    }
                } else {
                    render_footer(hint_rect, buf, footer_props);
                }
            }
        }
        let style = user_message_style();
        Block::default().style(style).render_ref(composer_rect, buf);
        if !textarea_rect.is_empty() {
            let prompt = if self.input_enabled {
                "›".bold()
            } else {
                "›".dim()
            };
            buf.set_span(
                textarea_rect.x - LIVE_PREFIX_COLS,
                textarea_rect.y,
                &prompt,
                textarea_rect.width,
            );
        }

        let mut state = self.textarea_state.borrow_mut();
        StatefulWidgetRef::render_ref(&(&self.textarea), textarea_rect, buf, &mut state);
        if self.textarea.text().is_empty() {
            let text = if self.input_enabled {
                self.placeholder_text.as_str().to_string()
            } else {
                self.input_disabled_placeholder
                    .as_deref()
                    .unwrap_or("Input disabled.")
                    .to_string()
            };
            let placeholder = Span::from(text).dim();
            Line::from(vec![placeholder]).render_ref(textarea_rect.inner(Margin::new(0, 0)), buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    use crate::app_event::AppEvent;
    use crate::app_event_sender::AppEventSender;
    use crate::bottom_pane::ChatComposer;
    use crate::bottom_pane::InputResult;
    use crate::bottom_pane::chat_composer::LARGE_PASTE_CHAR_THRESHOLD;
    use crate::bottom_pane::textarea::TextArea;
    use tokio::sync::mpsc::unbounded_channel;

    #[test]
    fn footer_hint_row_is_separated_from_composer() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut terminal = Terminal::new(TestBackend::new(40, 6)).expect("terminal");
        terminal
            .draw(|f| composer.render(f.area(), f.buffer_mut()))
            .expect("draw");
        insta::assert_snapshot!(terminal.backend());
    }

    fn snapshot_composer_state<F>(name: &str, enhanced_keys_supported: bool, setup: F)
    where
        F: FnOnce(&mut ChatComposer),
    {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let width = 100;
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            enhanced_keys_supported,
            "Assign new task to CodexPotter".to_string(),
            false,
        );
        setup(&mut composer);
        let footer_props = composer.footer_props();
        let footer_lines = footer_height(footer_props);
        let footer_spacing = ChatComposer::footer_spacing(footer_lines);
        let height = footer_lines + footer_spacing + 8;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| composer.render(f.area(), f.buffer_mut()))
            .unwrap();
        insta::assert_snapshot!(name, terminal.backend());
    }

    #[test]
    fn footer_mode_snapshots() {
        use crossterm::event::KeyCode;

        snapshot_composer_state("footer_mode_ctrl_c_quit", true, |composer| {
            composer.show_quit_shortcut_hint(key_hint::ctrl(KeyCode::Char('c')), true);
        });

        snapshot_composer_state("footer_mode_ctrl_c_interrupt", true, |composer| {
            composer.show_quit_shortcut_hint(key_hint::ctrl(KeyCode::Char('c')), true);
        });

        snapshot_composer_state("footer_mode_hidden_while_typing", true, |composer| {
            type_chars_humanlike(composer, &['h']);
        });
    }

    #[test]
    fn clear_for_ctrl_c_records_cleared_draft() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        composer.set_text_content("draft text".to_string());
        assert_eq!(composer.clear_for_ctrl_c(), Some("draft text".to_string()));
        assert!(composer.is_empty());

        let (result, needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(result, InputResult::None);
        assert!(needs_redraw);
        assert_eq!(composer.current_text(), "draft text");
    }

    #[test]
    fn question_mark_is_inserted_as_character() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        let (result, needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert_eq!(result, InputResult::None);
        assert!(needs_redraw);

        let _ = flush_after_paste_burst(&mut composer);

        assert_eq!(composer.textarea.text(), "?");
        assert_eq!(composer.footer_mode(), FooterMode::ContextOnly);
    }

    #[test]
    fn tab_inserts_tab_character_in_composer() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        type_chars_humanlike(&mut composer, &['a']);

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let _ = flush_after_paste_burst(&mut composer);

        assert_eq!(result, InputResult::None);
        assert_eq!(composer.current_text(), "a\t");
    }

    #[test]
    fn skill_popup_enter_inserts_skill_mention() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );
        composer.skills = vec![crate::skills_discovery::SkillMetadata {
            name: "my-skill".to_string(),
            description: "My test skill.".to_string(),
            short_description: Some("Short!".to_string()),
            interface: None,
            path: std::path::PathBuf::from("/tmp/my-skill/SKILL.md"),
            scope: crate::skills_discovery::SkillScope::User,
        }];

        composer.set_text_content("$".to_string());
        assert!(matches!(composer.active_popup, ActivePopup::Skill(_)));

        let (result, needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(result, InputResult::None);
        assert!(needs_redraw);

        assert_eq!(composer.current_text(), "$my-skill ");
        assert!(matches!(composer.active_popup, ActivePopup::None));
    }

    #[test]
    fn skill_popup_esc_dismisses_without_reopening() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );
        composer.skills = vec![crate::skills_discovery::SkillMetadata {
            name: "my-skill".to_string(),
            description: "My test skill.".to_string(),
            short_description: None,
            interface: None,
            path: std::path::PathBuf::from("/tmp/my-skill/SKILL.md"),
            scope: crate::skills_discovery::SkillScope::User,
        }];

        composer.set_text_content("$".to_string());
        assert!(matches!(composer.active_popup, ActivePopup::Skill(_)));

        let (result, needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(result, InputResult::None);
        assert!(needs_redraw);

        assert!(matches!(composer.active_popup, ActivePopup::None));
        // Still on the same `$` token; popup stays dismissed.
        composer.sync_popups();
        assert!(matches!(composer.active_popup, ActivePopup::None));
    }

    #[test]
    fn test_current_at_token_basic_cases() {
        let test_cases = vec![
            // Valid @ tokens
            ("@hello", 3, Some("hello".to_string()), "Basic ASCII token"),
            (
                "@file.txt",
                4,
                Some("file.txt".to_string()),
                "ASCII with extension",
            ),
            (
                "hello @world test",
                8,
                Some("world".to_string()),
                "ASCII token in middle",
            ),
            (
                "@test123",
                5,
                Some("test123".to_string()),
                "ASCII with numbers",
            ),
            // Unicode examples
            ("@İstanbul", 3, Some("İstanbul".to_string()), "Turkish text"),
            (
                "@testЙЦУ.rs",
                8,
                Some("testЙЦУ.rs".to_string()),
                "Mixed ASCII and Cyrillic",
            ),
            ("@あ", 2, Some("あ".to_string()), "Hiragana character"),
            ("@👍", 2, Some("👍".to_string()), "Emoji token"),
            // Invalid cases (should return None)
            ("hello", 2, None, "No @ symbol"),
            (
                "@",
                1,
                Some("".to_string()),
                "Only @ symbol triggers empty query",
            ),
            ("@ hello", 2, None, "@ followed by space"),
            ("test @ world", 6, None, "@ with spaces around"),
        ];

        for (input, cursor_pos, expected, description) in test_cases {
            let mut textarea = TextArea::new();
            textarea.insert_str(input);
            textarea.set_cursor(cursor_pos);

            let result = ChatComposer::current_at_token(&textarea);
            assert_eq!(
                result, expected,
                "Failed for case: {description} - input: '{input}', cursor: {cursor_pos}"
            );
        }
    }

    #[test]
    fn test_current_at_token_cursor_positions() {
        let test_cases = vec![
            // Different cursor positions within a token
            ("@test", 0, Some("test".to_string()), "Cursor at @"),
            ("@test", 1, Some("test".to_string()), "Cursor after @"),
            ("@test", 5, Some("test".to_string()), "Cursor at end"),
            // Multiple tokens - cursor determines which token
            ("@file1 @file2", 0, Some("file1".to_string()), "First token"),
            (
                "@file1 @file2",
                8,
                Some("file2".to_string()),
                "Second token",
            ),
            // Edge cases
            ("@", 0, Some("".to_string()), "Only @ symbol"),
            ("@a", 2, Some("a".to_string()), "Single character after @"),
            ("", 0, None, "Empty input"),
        ];

        for (input, cursor_pos, expected, description) in test_cases {
            let mut textarea = TextArea::new();
            textarea.insert_str(input);
            textarea.set_cursor(cursor_pos);

            let result = ChatComposer::current_at_token(&textarea);
            assert_eq!(
                result, expected,
                "Failed for cursor position case: {description} - input: '{input}', cursor: {cursor_pos}",
            );
        }
    }

    #[test]
    fn test_current_at_token_whitespace_boundaries() {
        let test_cases = vec![
            // Space boundaries
            (
                "aaa@aaa",
                4,
                None,
                "Connected @ token - no completion by design",
            ),
            (
                "aaa @aaa",
                5,
                Some("aaa".to_string()),
                "@ token after space",
            ),
            (
                "test @file.txt",
                7,
                Some("file.txt".to_string()),
                "@ token after space",
            ),
            // Full-width space boundaries
            (
                "test　@İstanbul",
                8,
                Some("İstanbul".to_string()),
                "@ token after full-width space",
            ),
            (
                "@ЙЦУ　@あ",
                10,
                Some("あ".to_string()),
                "Full-width space between Unicode tokens",
            ),
            // Tab and newline boundaries
            (
                "test\t@file",
                6,
                Some("file".to_string()),
                "@ token after tab",
            ),
        ];

        for (input, cursor_pos, expected, description) in test_cases {
            let mut textarea = TextArea::new();
            textarea.insert_str(input);
            textarea.set_cursor(cursor_pos);

            let result = ChatComposer::current_at_token(&textarea);
            assert_eq!(
                result, expected,
                "Failed for whitespace boundary case: {description} - input: '{input}', cursor: {cursor_pos}",
            );
        }
    }

    /// Behavior: if the ASCII path has a pending first char (flicker suppression) and a non-ASCII
    /// char arrives next, the pending ASCII char should still be preserved and the overall input
    /// should submit normally (i.e. we should not misclassify this as a paste burst).
    #[test]
    fn ascii_prefix_survives_non_ascii_followup() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        assert!(composer.is_in_paste_burst());

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('あ'), KeyModifiers::NONE));

        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Queued(text) => assert_eq!(text, "1あ"),
            _ => panic!("expected Queued"),
        }
    }

    /// Behavior: a single non-ASCII char should be inserted immediately (IME-friendly) and should
    /// not create any paste-burst state.
    #[test]
    fn non_ascii_char_inserts_immediately_without_burst_state() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('あ'), KeyModifiers::NONE));

        assert_eq!(composer.textarea.text(), "あ");
        assert!(!composer.is_in_paste_burst());
    }

    /// Behavior: while we're capturing a paste-like burst, Enter should be treated as a newline
    /// within the burst (not as "submit"), and the whole payload should flush as one paste.
    #[test]
    fn non_ascii_burst_buffers_enter_and_flushes_multiline() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        composer
            .paste_burst
            .begin_with_retro_grabbed(String::new(), Instant::now());

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('あ'), KeyModifiers::NONE));
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('い'), KeyModifiers::NONE));
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));

        assert!(composer.textarea.text().is_empty());
        let _ = flush_after_paste_burst(&mut composer);
        assert_eq!(composer.textarea.text(), "あい\nhi");
    }

    /// Behavior: a paste-like burst may include a full-width/ideographic space (U+3000). It should
    /// still be captured as a single paste payload and preserve the exact Unicode content.
    #[test]
    fn non_ascii_burst_preserves_ideographic_space_and_ascii() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        composer
            .paste_burst
            .begin_with_retro_grabbed(String::new(), Instant::now());

        for ch in ['あ', '　', 'い'] {
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for ch in ['h', 'i'] {
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        assert!(composer.textarea.text().is_empty());
        let _ = flush_after_paste_burst(&mut composer);
        assert_eq!(composer.textarea.text(), "あ　い\nhi");
    }

    /// Behavior: a large multi-line payload containing both non-ASCII and ASCII (e.g. "UTF-8",
    /// "Unicode") should be captured as a single paste-like burst, and Enter key events should
    /// become `\n` within the buffered content.
    #[test]
    fn non_ascii_burst_buffers_large_multiline_mixed_ascii_and_unicode() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        const LARGE_MIXED_PAYLOAD: &str = "Ralph loop: multi-round workflow\n\
Second line with emoji 👍\n\
Third line with accents: naive cafe\n\
\n\
Wide characters: あいうえお\n\
Mixed scripts: Жю Ωβ\n\
\n\
End of payload.";

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        // Force an active burst so the test doesn't depend on timing heuristics.
        composer
            .paste_burst
            .begin_with_retro_grabbed(String::new(), Instant::now());

        for ch in LARGE_MIXED_PAYLOAD.chars() {
            let code = if ch == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(ch)
            };
            let _ = composer.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
        }

        assert!(composer.textarea.text().is_empty());
        let _ = flush_after_paste_burst(&mut composer);
        assert_eq!(composer.textarea.text(), LARGE_MIXED_PAYLOAD);
    }

    /// Behavior: while a paste-like burst is active, Enter should not submit; it should insert a
    /// newline into the buffered payload and flush as a single paste later.
    #[test]
    fn ascii_burst_treats_enter_as_newline() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        // Force an active burst so this test doesn't depend on tight timing.
        composer
            .paste_burst
            .begin_with_retro_grabbed(String::new(), Instant::now());

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));

        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(result, InputResult::None),
            "Enter during a burst should insert newline, not submit"
        );

        for ch in ['t', 'h', 'e', 'r', 'e'] {
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }

        let _ = flush_after_paste_burst(&mut composer);
        assert_eq!(composer.textarea.text(), "hi\nthere");
    }

    /// Behavior: if a burst is buffering text and the user presses a non-char key, flush the
    /// buffered burst *before* applying that key so the buffer cannot get stuck.
    #[test]
    fn non_char_key_flushes_active_burst_before_input() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        // Force an active burst so we can deterministically buffer characters without relying on
        // timing.
        composer
            .paste_burst
            .begin_with_retro_grabbed(String::new(), Instant::now());

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        assert!(composer.textarea.text().is_empty());
        assert!(composer.is_in_paste_burst());

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(composer.textarea.text(), "hi");
        assert_eq!(composer.textarea.cursor(), 1);
        assert!(!composer.is_in_paste_burst());
    }

    /// Behavior: enabling `disable_paste_burst` flushes any held first character (flicker
    /// suppression) and then inserts subsequent chars immediately without creating burst state.
    #[test]
    fn disable_paste_burst_flushes_pending_first_char_and_inserts_immediately() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        // First ASCII char is normally held briefly. Flip the config mid-stream and ensure the
        // held char is not dropped.
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(composer.is_in_paste_burst());
        assert!(composer.textarea.text().is_empty());

        composer.set_disable_paste_burst(true);
        assert_eq!(composer.textarea.text(), "a");
        assert!(!composer.is_in_paste_burst());

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(composer.textarea.text(), "ab");
        assert!(!composer.is_in_paste_burst());
    }

    /// Behavior: a small explicit paste inserts text directly (no placeholder), and the submitted
    /// text matches what is visible in the textarea.
    #[test]
    fn handle_paste_small_inserts_text() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        let needs_redraw = composer.handle_paste("hello".to_string());
        assert!(needs_redraw);
        assert_eq!(composer.textarea.text(), "hello");
        assert!(composer.pending_pastes.is_empty());

        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Queued(text) => assert_eq!(text, "hello"),
            _ => panic!("expected Queued"),
        }
    }

    #[test]
    fn empty_enter_returns_none() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        // Ensure composer is empty and press Enter.
        assert!(composer.textarea.text().is_empty());
        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match result {
            InputResult::None => {}
            other => panic!("expected None for empty enter, got: {other:?}"),
        }
    }

    #[test]
    fn slash_prefixed_text_submits_verbatim() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        composer.textarea.set_text_clearing_elements("/help");
        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(InputResult::Queued("/help".to_string()), result);
    }

    /// Behavior: a large explicit paste inserts a placeholder into the textarea, stores the full
    /// content in `pending_pastes`, and expands the placeholder to the full content on submit.
    #[test]
    fn handle_paste_large_uses_placeholder_and_replaces_on_submit() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        let large = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 10);
        let needs_redraw = composer.handle_paste(large.clone());
        assert!(needs_redraw);
        let placeholder = format!("[Pasted Content {} chars]", large.chars().count());
        assert_eq!(composer.textarea.text(), placeholder);
        assert_eq!(composer.pending_pastes.len(), 1);
        assert_eq!(composer.pending_pastes[0].0, placeholder);
        assert_eq!(composer.pending_pastes[0].1, large);

        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            InputResult::Queued(text) => assert_eq!(text, large),
            _ => panic!("expected Queued"),
        }
        assert!(composer.pending_pastes.is_empty());
    }

    /// Behavior: editing that removes a paste placeholder should also clear the associated
    /// `pending_pastes` entry so it cannot be submitted accidentally.
    #[test]
    fn edit_clears_pending_paste() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let large = "y".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1);
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        composer.handle_paste(large);
        assert_eq!(composer.pending_pastes.len(), 1);

        // Any edit that removes the placeholder should clear pending_paste
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(composer.pending_pastes.is_empty());
    }

    #[test]
    fn ui_snapshots() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut terminal = match Terminal::new(TestBackend::new(100, 10)) {
            Ok(t) => t,
            Err(e) => panic!("Failed to create terminal: {e}"),
        };

        let test_cases = vec![
            ("empty", None),
            ("small", Some("short".to_string())),
            ("large", Some("z".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5))),
            ("multiple_pastes", None),
            ("backspace_after_pastes", None),
        ];

        for (name, input) in test_cases {
            // Create a fresh composer for each test case
            let mut composer = ChatComposer::new(
                true,
                sender.clone(),
                false,
                "Assign new task to CodexPotter".to_string(),
                false,
            );

            if let Some(text) = input {
                composer.handle_paste(text);
            } else if name == "multiple_pastes" {
                // First large paste
                composer.handle_paste("x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 3));
                // Second large paste
                composer.handle_paste("y".repeat(LARGE_PASTE_CHAR_THRESHOLD + 7));
                // Small paste
                composer.handle_paste(" another short paste".to_string());
            } else if name == "backspace_after_pastes" {
                // Three large pastes
                composer.handle_paste("a".repeat(LARGE_PASTE_CHAR_THRESHOLD + 2));
                composer.handle_paste("b".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4));
                composer.handle_paste("c".repeat(LARGE_PASTE_CHAR_THRESHOLD + 6));
                // Move cursor to end and press backspace
                composer.textarea.set_cursor(composer.textarea.text().len());
                composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
            }

            terminal
                .draw(|f| composer.render(f.area(), f.buffer_mut()))
                .unwrap_or_else(|e| panic!("Failed to draw {name} composer: {e}"));

            insta::assert_snapshot!(name, terminal.backend());
        }
    }

    fn flush_after_paste_burst(composer: &mut ChatComposer) -> bool {
        std::thread::sleep(PasteBurst::recommended_active_flush_delay());
        composer.flush_paste_burst_if_due()
    }

    // Test helper: simulate human typing with a brief delay and flush the paste-burst buffer
    fn type_chars_humanlike(composer: &mut ChatComposer, chars: &[char]) {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;
        for &ch in chars {
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            std::thread::sleep(ChatComposer::recommended_paste_flush_delay());
            let _ = composer.flush_paste_burst_if_due();
        }
    }

    /// Behavior: multiple paste operations can coexist; placeholders should be expanded to their
    /// original content on submission.
    #[test]
    fn test_multiple_pastes_submission() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        // Define test cases: (paste content, is_large)
        let test_cases = [
            ("x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 3), true),
            (" and ".to_string(), false),
            ("y".repeat(LARGE_PASTE_CHAR_THRESHOLD + 7), true),
        ];

        // Expected states after each paste
        let mut expected_text = String::new();
        let mut expected_pending_count = 0;

        // Apply all pastes and build expected state
        let states: Vec<_> = test_cases
            .iter()
            .map(|(content, is_large)| {
                composer.handle_paste(content.clone());
                if *is_large {
                    let placeholder = format!("[Pasted Content {} chars]", content.chars().count());
                    expected_text.push_str(&placeholder);
                    expected_pending_count += 1;
                } else {
                    expected_text.push_str(content);
                }
                (expected_text.clone(), expected_pending_count)
            })
            .collect();

        // Verify all intermediate states were correct
        assert_eq!(
            states,
            vec![
                (
                    format!("[Pasted Content {} chars]", test_cases[0].0.chars().count()),
                    1
                ),
                (
                    format!(
                        "[Pasted Content {} chars] and ",
                        test_cases[0].0.chars().count()
                    ),
                    1
                ),
                (
                    format!(
                        "[Pasted Content {} chars] and [Pasted Content {} chars]",
                        test_cases[0].0.chars().count(),
                        test_cases[2].0.chars().count()
                    ),
                    2
                ),
            ]
        );

        // Submit and verify final expansion
        let (result, _) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        if let InputResult::Queued(text) = result {
            assert_eq!(text, format!("{} and {}", test_cases[0].0, test_cases[2].0));
        } else {
            panic!("expected Queued");
        }
    }

    #[test]
    fn test_placeholder_deletion() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        // Define test cases: (content, is_large)
        let test_cases = [
            ("a".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5), true),
            (" and ".to_string(), false),
            ("b".repeat(LARGE_PASTE_CHAR_THRESHOLD + 6), true),
        ];

        // Apply all pastes
        let mut current_pos = 0;
        let states: Vec<_> = test_cases
            .iter()
            .map(|(content, is_large)| {
                composer.handle_paste(content.clone());
                if *is_large {
                    let placeholder = format!("[Pasted Content {} chars]", content.chars().count());
                    current_pos += placeholder.len();
                } else {
                    current_pos += content.len();
                }
                (
                    composer.textarea.text().to_string(),
                    composer.pending_pastes.len(),
                    current_pos,
                )
            })
            .collect();

        // Delete placeholders one by one and collect states
        let mut deletion_states = vec![];

        // First deletion
        composer.textarea.set_cursor(states[0].2);
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        deletion_states.push((
            composer.textarea.text().to_string(),
            composer.pending_pastes.len(),
        ));

        // Second deletion
        composer.textarea.set_cursor(composer.textarea.text().len());
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        deletion_states.push((
            composer.textarea.text().to_string(),
            composer.pending_pastes.len(),
        ));

        // Verify all states
        assert_eq!(
            deletion_states,
            vec![
                (" and [Pasted Content 1006 chars]".to_string(), 1),
                (" and ".to_string(), 0),
            ]
        );
    }

    /// Behavior: if multiple large pastes share the same placeholder label (same char count),
    /// deleting one placeholder removes only its corresponding `pending_pastes` entry.
    #[test]
    fn deleting_duplicate_length_pastes_removes_only_target() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        let paste = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
        let placeholder_base = format!("[Pasted Content {} chars]", paste.chars().count());
        let placeholder_second = format!("{placeholder_base} #2");

        composer.handle_paste(paste.clone());
        composer.handle_paste(paste.clone());
        assert_eq!(
            composer.textarea.text(),
            format!("{placeholder_base}{placeholder_second}")
        );
        assert_eq!(composer.pending_pastes.len(), 2);

        composer.textarea.set_cursor(composer.textarea.text().len());
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

        assert_eq!(composer.textarea.text(), placeholder_base);
        assert_eq!(composer.pending_pastes.len(), 1);
        assert_eq!(composer.pending_pastes[0].0, placeholder_base);
        assert_eq!(composer.pending_pastes[0].1, paste);
    }

    /// Behavior: large-paste placeholder numbering does not get reused after deletion, so a new
    /// paste of the same length gets a new unique placeholder label.
    #[test]
    fn large_paste_numbering_does_not_reuse_after_deletion() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        let paste = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
        let base = format!("[Pasted Content {} chars]", paste.chars().count());
        let second = format!("{base} #2");
        let third = format!("{base} #3");

        composer.handle_paste(paste.clone());
        composer.handle_paste(paste.clone());
        assert_eq!(composer.textarea.text(), format!("{base}{second}"));

        composer.textarea.set_cursor(base.len());
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(composer.textarea.text(), second);
        assert_eq!(composer.pending_pastes.len(), 1);
        assert_eq!(composer.pending_pastes[0].0, second);

        composer.textarea.set_cursor(composer.textarea.text().len());
        composer.handle_paste(paste);

        assert_eq!(composer.textarea.text(), format!("{second}{third}"));
        assert_eq!(composer.pending_pastes.len(), 2);
        assert_eq!(composer.pending_pastes[0].0, second);
        assert_eq!(composer.pending_pastes[1].0, third);
    }

    #[test]
    fn test_partial_placeholder_deletion() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        // Define test cases: (cursor_position_from_end, expected_pending_count)
        let test_cases = [
            5, // Delete from middle - should clear tracking
            0, // Delete from end - should clear tracking
        ];

        let paste = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
        let placeholder = format!("[Pasted Content {} chars]", paste.chars().count());

        let states: Vec<_> = test_cases
            .into_iter()
            .map(|pos_from_end| {
                composer.handle_paste(paste.clone());
                composer
                    .textarea
                    .set_cursor(placeholder.len() - pos_from_end);
                composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
                let result = (
                    composer.textarea.text().contains(&placeholder),
                    composer.pending_pastes.len(),
                );
                composer.textarea.set_text_clearing_elements("");
                result
            })
            .collect();

        assert_eq!(
            states,
            vec![
                (false, 0), // After deleting from middle
                (false, 0), // After deleting from end
            ]
        );
    }

    /// Behavior: the first fast ASCII character is held briefly to avoid flicker; if no burst
    /// follows, it should eventually flush as normal typed input (not as a paste).
    #[test]
    fn pending_first_ascii_char_flushes_as_typed() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(composer.is_in_paste_burst());
        assert!(composer.textarea.text().is_empty());

        std::thread::sleep(ChatComposer::recommended_paste_flush_delay());
        let flushed = composer.flush_paste_burst_if_due();
        assert!(flushed, "expected pending first char to flush");
        assert_eq!(composer.textarea.text(), "h");
        assert!(!composer.is_in_paste_burst());
    }

    /// Behavior: fast "paste-like" ASCII input should buffer and then flush as a single paste. If
    /// the payload is small, it should insert directly (no placeholder).
    #[test]
    fn burst_paste_fast_small_buffers_and_flushes_on_stop() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        let count = 32;
        for _ in 0..count {
            let _ =
                composer.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
            assert!(
                composer.is_in_paste_burst(),
                "expected active paste burst during fast typing"
            );
            assert!(
                composer.textarea.text().is_empty(),
                "text should not appear during burst"
            );
        }

        assert!(
            composer.textarea.text().is_empty(),
            "text should remain empty until flush"
        );
        let flushed = flush_after_paste_burst(&mut composer);
        assert!(flushed, "expected buffered text to flush after stop");
        assert_eq!(composer.textarea.text(), "a".repeat(count));
        assert!(
            composer.pending_pastes.is_empty(),
            "no placeholder for small burst"
        );
    }

    /// Behavior: fast "paste-like" ASCII input should buffer and then flush as a single paste. If
    /// the payload is large, it should insert a placeholder and defer the full text until submit.
    #[test]
    fn burst_paste_fast_large_inserts_placeholder_on_flush() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        let count = LARGE_PASTE_CHAR_THRESHOLD + 1; // > threshold to trigger placeholder
        for _ in 0..count {
            let _ =
                composer.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        }

        // Nothing should appear until we stop and flush
        assert!(composer.textarea.text().is_empty());
        let flushed = flush_after_paste_burst(&mut composer);
        assert!(flushed, "expected flush after stopping fast input");

        let expected_placeholder = format!("[Pasted Content {count} chars]");
        assert_eq!(composer.textarea.text(), expected_placeholder);
        assert_eq!(composer.pending_pastes.len(), 1);
        assert_eq!(composer.pending_pastes[0].0, expected_placeholder);
        assert_eq!(composer.pending_pastes[0].1.len(), count);
        assert!(composer.pending_pastes[0].1.chars().all(|c| c == 'x'));
    }

    /// Behavior: human-like typing (with delays between chars) should not be classified as a paste
    /// burst. Characters should appear immediately and should not trigger a paste placeholder.
    #[test]
    fn humanlike_typing_1000_chars_appears_live_no_placeholder() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        let count = LARGE_PASTE_CHAR_THRESHOLD; // 1000 in current config
        let chars: Vec<char> = vec!['z'; count];
        type_chars_humanlike(&mut composer, &chars);

        assert_eq!(composer.textarea.text(), "z".repeat(count));
        assert!(composer.pending_pastes.is_empty());
    }

    #[test]
    fn apply_external_edit_replaces_text_and_clears_pending_pastes() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        composer.handle_paste("x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1));
        assert!(!composer.pending_pastes.is_empty());
        assert!(!composer.large_paste_counters.is_empty());

        composer.apply_external_edit("Edited text".to_string());

        assert_eq!(composer.current_text(), "Edited text".to_string());
        assert!(composer.pending_pastes.is_empty());
        assert!(composer.large_paste_counters.is_empty());
        assert_eq!(composer.textarea.cursor(), composer.current_text().len());
    }

    #[test]
    fn current_text_with_pending_expands_placeholders() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        let placeholder = "[Pasted Content 5 chars]".to_string();
        composer.textarea.insert_element(&placeholder);
        composer
            .pending_pastes
            .push((placeholder.clone(), "hello".to_string()));

        assert_eq!(
            composer.current_text_with_pending(),
            "hello".to_string(),
            "placeholder should expand to actual text"
        );
    }

    #[test]
    fn input_disabled_ignores_keypresses_and_hides_cursor() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        composer.set_text_content("hello".to_string());
        composer.set_input_enabled(false, Some("Input disabled for test.".to_string()));

        let (result, needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

        assert_eq!(result, InputResult::None);
        assert!(!needs_redraw);
        assert_eq!(composer.current_text(), "hello");

        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 5,
        };
        assert_eq!(composer.cursor_pos(area), None);
    }

    #[test]
    fn take_and_restore_draft_preserves_text_and_cursor() {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        composer.handle_paste("hello world".into());
        composer.textarea.set_cursor(5);

        let draft = composer.take_draft().expect("expected draft");

        let (tx2, _rx2) = unbounded_channel::<AppEvent>();
        let sender2 = AppEventSender::new(tx2);
        let mut restored = ChatComposer::new(
            true,
            sender2,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );
        restored.restore_draft(draft);

        assert_eq!(restored.textarea.text(), "hello world");
        assert_eq!(restored.textarea.cursor(), 5);
    }

    #[test]
    fn take_and_restore_draft_preserves_large_paste_placeholder_semantics() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;

        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            true,
            sender,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );

        let paste = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
        composer.handle_paste(paste);
        let placeholder = composer.textarea.text().to_string();
        assert!(placeholder.starts_with("[Pasted Content "));
        assert_eq!(composer.pending_pastes.len(), 1);

        let draft = composer.take_draft().expect("expected draft");

        let (tx2, _rx2) = unbounded_channel::<AppEvent>();
        let sender2 = AppEventSender::new(tx2);
        let mut restored = ChatComposer::new(
            true,
            sender2,
            false,
            "Assign new task to CodexPotter".to_string(),
            false,
        );
        restored.restore_draft(draft);

        assert_eq!(restored.textarea.text(), placeholder);
        assert_eq!(restored.pending_pastes.len(), 1);

        // Backspace should delete the placeholder atomically (it is stored as an element).
        restored.textarea.set_cursor(restored.textarea.text().len());
        let _ = restored.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(restored.textarea.text().is_empty());
        assert!(restored.pending_pastes.is_empty());
    }
}
