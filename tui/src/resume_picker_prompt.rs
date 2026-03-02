//! Resume picker prompt.
//!
//! # Divergence from upstream Codex TUI
//!
//! - The final column is `User Request` (CodexPotter project prompt/title) instead of upstream
//!   `Conversation`.
//! - The picker operates on an in-memory list provided by the CLI (no pagination / fork action).

use std::path::PathBuf;
use std::time::SystemTime;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::prelude::Widget as _;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use tokio_stream::StreamExt;
use unicode_width::UnicodeWidthStr;

use crate::key_hint;
use crate::text_formatting::truncate_text;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;

/// A single row in the resume picker UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumePickerRow {
    /// A project path accepted by `codex-potter resume <PROJECT_PATH>`.
    pub project_path: PathBuf,
    /// The user-facing prompt/title to render in the picker.
    pub user_request: String,
    /// The created timestamp, used for display and sorting.
    pub created_at: SystemTime,
    /// The last updated timestamp, used for display and sorting (newest first).
    pub updated_at: SystemTime,
    /// Git branch recorded in the project progress file front matter.
    pub git_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumePickerOutcome {
    StartFresh,
    Resume(PathBuf),
    Exit,
}

pub async fn run_resume_picker_prompt_with_tui(
    tui: &mut Tui,
    rows: Vec<ResumePickerRow>,
) -> anyhow::Result<ResumePickerOutcome> {
    let alt = AltScreenGuard::enter(tui);

    let mut screen = ResumePickerScreen::new(alt.tui.frame_requester(), rows, SystemTime::now());
    if let Ok(size) = alt.tui.terminal.size() {
        screen.set_view_rows(size.height.saturating_sub(4) as usize);
    }
    alt.tui.draw(u16::MAX, |frame| {
        frame.render_widget_ref(&screen, frame.area());
    })?;

    let events = alt.tui.event_stream();
    tokio::pin!(events);

    while !screen.is_done() {
        let Some(event) = events.next().await else {
            break;
        };
        match event {
            TuiEvent::Key(key_event) => screen.handle_key(key_event),
            TuiEvent::Paste(_) => {}
            TuiEvent::Draw => {
                if let Ok(size) = alt.tui.terminal.size() {
                    screen.set_view_rows(size.height.saturating_sub(4) as usize);
                }
                alt.tui.draw(u16::MAX, |frame| {
                    frame.render_widget_ref(&screen, frame.area());
                })?;
            }
        }
    }

    Ok(screen
        .take_outcome()
        .unwrap_or(ResumePickerOutcome::StartFresh))
}

/// RAII guard that ensures we leave the alt-screen on scope exit.
struct AltScreenGuard<'a> {
    tui: &'a mut Tui,
}

impl<'a> AltScreenGuard<'a> {
    fn enter(tui: &'a mut Tui) -> Self {
        let _ = tui.enter_alt_screen();
        Self { tui }
    }
}

impl Drop for AltScreenGuard<'_> {
    fn drop(&mut self) {
        let _ = self.tui.leave_alt_screen();
    }
}

#[derive(Clone)]
struct ColumnMetrics {
    max_created_width: usize,
    max_updated_width: usize,
    max_branch_width: usize,
    labels: Vec<(String, String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResumeSortKey {
    CreatedAt,
    UpdatedAt,
}

impl ResumeSortKey {
    fn toggle(self) -> Self {
        match self {
            Self::CreatedAt => Self::UpdatedAt,
            Self::UpdatedAt => Self::CreatedAt,
        }
    }
}

fn sort_key_label(sort_key: ResumeSortKey) -> &'static str {
    match sort_key {
        ResumeSortKey::CreatedAt => "Created",
        ResumeSortKey::UpdatedAt => "Updated",
    }
}

struct ResumePickerScreen {
    request_frame: FrameRequester,
    now: SystemTime,

    all_rows: Vec<ResumePickerRow>,
    all_rows_lower: Vec<String>,

    query: String,
    sort_key: ResumeSortKey,
    selected: usize,
    scroll_top: usize,
    view_rows: usize,

    filtered_indices: Vec<usize>,
    filtered_metrics: ColumnMetrics,

    outcome: Option<ResumePickerOutcome>,
}

impl ResumePickerScreen {
    fn new(request_frame: FrameRequester, rows: Vec<ResumePickerRow>, now: SystemTime) -> Self {
        let all_rows_lower = rows
            .iter()
            .map(|row| {
                format!(
                    "{}\n{}\n{}",
                    row.user_request,
                    row.git_branch.as_deref().unwrap_or_default(),
                    row.project_path.to_string_lossy(),
                )
                .to_lowercase()
            })
            .collect();

        let mut screen = Self {
            request_frame,
            now,
            all_rows: rows,
            all_rows_lower,
            query: String::new(),
            sort_key: ResumeSortKey::UpdatedAt,
            selected: 0,
            scroll_top: 0,
            view_rows: 0,
            filtered_indices: Vec::new(),
            filtered_metrics: ColumnMetrics {
                max_created_width: UnicodeWidthStr::width("Created"),
                max_updated_width: UnicodeWidthStr::width("Updated"),
                max_branch_width: UnicodeWidthStr::width("Branch"),
                labels: Vec::new(),
            },
            outcome: None,
        };
        screen.recompute_filter();
        screen
    }

    fn is_done(&self) -> bool {
        self.outcome.is_some()
    }

    fn take_outcome(&mut self) -> Option<ResumePickerOutcome> {
        self.outcome.take()
    }

    fn handle_key(&mut self, key_event: KeyEvent) {
        if key_event.kind == KeyEventKind::Release {
            return;
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c'))
        {
            self.outcome = Some(ResumePickerOutcome::Exit);
            self.request_frame.schedule_frame();
            return;
        }

        match key_event.code {
            KeyCode::Esc => {
                self.outcome = Some(ResumePickerOutcome::StartFresh);
                self.request_frame.schedule_frame();
            }
            KeyCode::Enter => {
                if let Some(row) = self.selected_row() {
                    self.outcome = Some(ResumePickerOutcome::Resume(row.project_path.clone()));
                    self.request_frame.schedule_frame();
                }
            }
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.page_selection(-1),
            KeyCode::PageDown => self.page_selection(1),
            KeyCode::Tab => self.toggle_sort_key(),
            KeyCode::Backspace => {
                if self.query.pop().is_some() {
                    self.recompute_filter();
                }
            }
            KeyCode::Char(ch) => {
                if !key_hint::has_ctrl_or_alt(key_event.modifiers) {
                    self.query.push(ch);
                    self.recompute_filter();
                }
            }
            _ => {}
        }
    }

    fn selected_row(&self) -> Option<&ResumePickerRow> {
        let row_index = self.filtered_indices.get(self.selected).copied()?;
        self.all_rows.get(row_index)
    }

    fn set_view_rows(&mut self, view_rows: usize) {
        let view_rows = view_rows.max(1);
        if self.view_rows != view_rows {
            self.view_rows = view_rows;
            self.ensure_selected_visible();
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.filtered_indices.is_empty() {
            return;
        }

        let new_selected = if delta < 0 {
            self.selected
                .saturating_sub(usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX))
        } else {
            let delta = usize::try_from(delta).unwrap_or(usize::MAX);
            self.selected.saturating_add(delta)
        };
        let max = self.filtered_indices.len().saturating_sub(1);
        let clamped = new_selected.min(max);
        if clamped != self.selected {
            self.selected = clamped;
            self.ensure_selected_visible();
            self.request_frame.schedule_frame();
        }
    }

    fn page_selection(&mut self, pages: i32) {
        if self.filtered_indices.is_empty() {
            return;
        }

        let view = self.view_rows.max(1);
        let delta =
            view.saturating_mul(usize::try_from(pages.unsigned_abs()).unwrap_or(usize::MAX));
        self.move_selection(if pages < 0 {
            -i32::try_from(delta).unwrap_or(i32::MAX)
        } else {
            i32::try_from(delta).unwrap_or(i32::MAX)
        });
    }

    fn ensure_selected_visible(&mut self) {
        let view = self.view_rows.max(1);
        if self.selected < self.scroll_top {
            self.scroll_top = self.selected;
            return;
        }
        if self.selected >= self.scroll_top.saturating_add(view) {
            self.scroll_top = self.selected.saturating_add(1).saturating_sub(view);
        }
    }

    fn recompute_filter(&mut self) {
        self.recompute_filter_preserving_selection(None);
    }

    fn toggle_sort_key(&mut self) {
        let selected = self.selected_row().map(|row| row.project_path.clone());
        self.sort_key = self.sort_key.toggle();
        self.recompute_filter_preserving_selection(selected.as_ref());
    }

    fn recompute_filter_preserving_selection(&mut self, selected: Option<&PathBuf>) {
        let needle = self.query.to_lowercase();
        let mut filtered: Vec<usize> = Vec::new();
        if needle.is_empty() {
            filtered.extend(0..self.all_rows.len());
        } else {
            for (idx, value) in self.all_rows_lower.iter().enumerate() {
                if value.contains(needle.as_str()) {
                    filtered.push(idx);
                }
            }
        }

        filtered.sort_by(|a, b| match self.sort_key {
            ResumeSortKey::CreatedAt => self
                .all_rows
                .get(*b)
                .map(|row| row.created_at)
                .cmp(&self.all_rows.get(*a).map(|row| row.created_at))
                .then_with(|| a.cmp(b)),
            ResumeSortKey::UpdatedAt => self
                .all_rows
                .get(*b)
                .map(|row| row.updated_at)
                .cmp(&self.all_rows.get(*a).map(|row| row.updated_at))
                .then_with(|| a.cmp(b)),
        });

        self.filtered_indices = filtered;
        self.selected = selected
            .and_then(|selected| {
                self.filtered_indices.iter().position(|idx| {
                    self.all_rows
                        .get(*idx)
                        .is_some_and(|row| row.project_path == *selected)
                })
            })
            .unwrap_or(0);
        self.scroll_top = 0;
        self.ensure_selected_visible();
        self.filtered_metrics =
            calculate_column_metrics(&self.all_rows, &self.filtered_indices, self.now);
        self.request_frame.schedule_frame();
    }
}

impl WidgetRef for &ResumePickerScreen {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let [header, search, columns, list, hint] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(area.height.saturating_sub(4)),
            Constraint::Length(1),
        ])
        .areas(area);

        // Header
        let header_line: Line = vec![
            "Resume a previous session".bold().cyan(),
            "  ".into(),
            "Sort:".dim(),
            " ".into(),
            sort_key_label(self.sort_key).magenta(),
        ]
        .into();
        Paragraph::new(header_line).render(header, buf);

        // Search line
        let q = if self.query.is_empty() {
            "Type to search".dim().to_string()
        } else {
            format!("Search: {}", self.query)
        };
        Paragraph::new(Line::from(q)).render(search, buf);

        render_column_headers(buf, columns, &self.filtered_metrics);
        render_list(buf, list, self, &self.filtered_metrics);

        // Hint line
        let hint_line: Line = vec![
            key_hint::plain(KeyCode::Enter).into(),
            " resume ".dim(),
            "  ".dim(),
            key_hint::plain(KeyCode::Esc).into(),
            " new ".dim(),
            "  ".dim(),
            key_hint::ctrl(KeyCode::Char('c')).into(),
            " quit ".dim(),
            "  ".dim(),
            key_hint::plain(KeyCode::Tab).into(),
            " sort ".dim(),
            "  ".dim(),
            key_hint::plain(KeyCode::Up).into(),
            "/".dim(),
            key_hint::plain(KeyCode::Down).into(),
            " browse".dim(),
        ]
        .into();
        Paragraph::new(hint_line).render(hint, buf);
    }
}

fn calculate_column_metrics(
    all_rows: &[ResumePickerRow],
    filtered_indices: &[usize],
    now: SystemTime,
) -> ColumnMetrics {
    fn right_elide(s: &str, max: usize) -> String {
        if s.chars().count() <= max {
            return s.to_string();
        }
        if max <= 1 {
            return "…".to_string();
        }
        let tail_len = max - 1;
        let tail: String = s
            .chars()
            .rev()
            .take(tail_len)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("…{tail}")
    }

    let mut labels: Vec<(String, String, String)> = Vec::with_capacity(filtered_indices.len());
    let mut max_created_width = UnicodeWidthStr::width("Created");
    let mut max_updated_width = UnicodeWidthStr::width("Updated");
    let mut max_branch_width = UnicodeWidthStr::width("Branch");

    for &idx in filtered_indices {
        let row = &all_rows[idx];
        let created = human_time_ago(row.created_at, now);
        let updated = human_time_ago(row.updated_at, now);
        let branch_raw = row.git_branch.clone().unwrap_or_default();
        let branch = right_elide(&branch_raw, 24);
        max_created_width = max_created_width.max(UnicodeWidthStr::width(created.as_str()));
        max_updated_width = max_updated_width.max(UnicodeWidthStr::width(updated.as_str()));
        max_branch_width = max_branch_width.max(UnicodeWidthStr::width(branch.as_str()));
        labels.push((created, updated, branch));
    }

    ColumnMetrics {
        max_created_width,
        max_updated_width,
        max_branch_width,
        labels,
    }
}

fn human_time_ago(ts: SystemTime, now: SystemTime) -> String {
    let delta = now.duration_since(ts).unwrap_or_default();
    let secs = i64::try_from(delta.as_secs()).unwrap_or(i64::MAX);
    if secs < 60 {
        let n = secs.max(0);
        if n == 1 {
            format!("{n} second ago")
        } else {
            format!("{n} seconds ago")
        }
    } else if secs < 60 * 60 {
        let m = secs / 60;
        if m == 1 {
            format!("{m} minute ago")
        } else {
            format!("{m} minutes ago")
        }
    } else if secs < 60 * 60 * 24 {
        let h = secs / 3600;
        if h == 1 {
            format!("{h} hour ago")
        } else {
            format!("{h} hours ago")
        }
    } else {
        let d = secs / (60 * 60 * 24);
        if d == 1 {
            format!("{d} day ago")
        } else {
            format!("{d} days ago")
        }
    }
}

fn render_column_headers(buf: &mut Buffer, area: Rect, metrics: &ColumnMetrics) {
    if area.height == 0 {
        return;
    }

    let mut spans: Vec<Span> = vec!["  ".into()];
    let created_label = format!(
        "{text:<width$}",
        text = "Created",
        width = metrics.max_created_width
    );
    spans.push(Span::from(created_label).bold());
    spans.push("  ".into());

    let updated_label = format!(
        "{text:<width$}",
        text = "Updated",
        width = metrics.max_updated_width
    );
    spans.push(Span::from(updated_label).bold());
    spans.push("  ".into());

    let branch_label = format!(
        "{text:<width$}",
        text = "Branch",
        width = metrics.max_branch_width
    );
    spans.push(Span::from(branch_label).bold());
    spans.push("  ".into());

    spans.push("User Request".bold());

    Paragraph::new(Line::from(spans)).render(area, buf);
}

fn render_list(buf: &mut Buffer, area: Rect, screen: &ResumePickerScreen, metrics: &ColumnMetrics) {
    if area.height == 0 {
        return;
    }

    let rows = &screen.filtered_indices;
    if rows.is_empty() {
        let message = if screen.query.is_empty() {
            "No sessions yet".italic().dim()
        } else {
            "No results for your search".italic().dim()
        };
        Paragraph::new(Line::from(vec![message])).render(area, buf);
        return;
    }

    let capacity = area.height as usize;
    let start = screen.scroll_top.min(rows.len().saturating_sub(1));
    let end = rows.len().min(start + capacity);

    let max_created_width = metrics.max_created_width;
    let max_updated_width = metrics.max_updated_width;
    let max_branch_width = metrics.max_branch_width;

    let mut y = area.y;
    for (idx, (&row_idx, (created_label, updated_label, branch_label))) in rows[start..end]
        .iter()
        .zip(metrics.labels[start..end].iter())
        .enumerate()
    {
        let row = &screen.all_rows[row_idx];
        let is_sel = start + idx == screen.selected;
        let marker = if is_sel { "> ".bold() } else { "  ".into() };
        let marker_width = 2usize;

        let created_span = Span::from(format!("{created_label:<max_created_width$}")).dim();
        let updated_span = Span::from(format!("{updated_label:<max_updated_width$}")).dim();
        let branch_span = if branch_label.is_empty() {
            Span::from(format!(
                "{empty:<width$}",
                empty = "-",
                width = max_branch_width
            ))
            .dim()
        } else {
            Span::from(format!("{branch_label:<max_branch_width$}")).cyan()
        };

        let mut preview_width = area.width as usize;
        preview_width = preview_width.saturating_sub(marker_width);
        preview_width = preview_width.saturating_sub(max_created_width + 2);
        preview_width = preview_width.saturating_sub(max_updated_width + 2);
        preview_width = preview_width.saturating_sub(max_branch_width + 2);

        let preview = truncate_text(row.user_request.as_str(), preview_width);
        let line: Line = vec![
            marker,
            created_span,
            "  ".into(),
            updated_span,
            "  ".into(),
            branch_span,
            "  ".into(),
            preview.into(),
        ]
        .into();

        Paragraph::new(line).render(Rect::new(area.x, y, area.width, 1), buf);
        y = y.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_backend::VT100Backend;
    use insta::assert_snapshot;
    use ratatui::Terminal;
    use std::time::Duration;

    #[test]
    fn resume_picker_empty_screen_snapshot() {
        let backend = VT100Backend::new(80, 9);
        let mut terminal = Terminal::new(backend).expect("create terminal");

        let screen =
            ResumePickerScreen::new(FrameRequester::test_dummy(), vec![], SystemTime::UNIX_EPOCH);
        terminal
            .draw(|frame| {
                WidgetRef::render_ref(&&screen, frame.area(), frame.buffer_mut());
            })
            .expect("draw");

        assert_snapshot!(
            "resume_picker_empty_vt100",
            terminal.backend().vt100().screen().contents()
        );
    }

    #[test]
    fn resume_picker_table_screen_snapshot() {
        let backend = VT100Backend::new(80, 9);
        let mut terminal = Terminal::new(backend).expect("create terminal");

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let rows = vec![
            ResumePickerRow {
                project_path: PathBuf::from("/tmp/a"),
                user_request: "Fix resume picker timestamps".to_string(),
                created_at: now - Duration::from_secs(3 * 24 * 60 * 60),
                updated_at: now - Duration::from_secs(42),
                git_branch: None,
            },
            ResumePickerRow {
                project_path: PathBuf::from("/tmp/b"),
                user_request: "Investigate lazy pagination cap".to_string(),
                created_at: now - Duration::from_secs(24 * 60 * 60),
                updated_at: now - Duration::from_secs(35 * 60),
                git_branch: Some("feature/resume".to_string()),
            },
            ResumePickerRow {
                project_path: PathBuf::from("/tmp/c"),
                user_request: "Explain the codebase".to_string(),
                created_at: now - Duration::from_secs(2 * 60 * 60),
                updated_at: now - Duration::from_secs(2 * 60 * 60),
                git_branch: Some("main".to_string()),
            },
        ];

        let mut screen = ResumePickerScreen::new(FrameRequester::test_dummy(), rows, now);
        screen.set_view_rows(5);
        screen.selected = 1;
        screen.ensure_selected_visible();

        terminal
            .draw(|frame| {
                WidgetRef::render_ref(&&screen, frame.area(), frame.buffer_mut());
            })
            .expect("draw");

        assert_snapshot!(
            "resume_picker_table_vt100",
            terminal.backend().vt100().screen().contents()
        );
    }

    #[test]
    fn resume_picker_table_sorted_by_created_screen_snapshot() {
        let backend = VT100Backend::new(80, 9);
        let mut terminal = Terminal::new(backend).expect("create terminal");

        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let rows = vec![
            ResumePickerRow {
                project_path: PathBuf::from("/tmp/a"),
                user_request: "Fix resume picker timestamps".to_string(),
                created_at: now - Duration::from_secs(3 * 24 * 60 * 60),
                updated_at: now - Duration::from_secs(42),
                git_branch: None,
            },
            ResumePickerRow {
                project_path: PathBuf::from("/tmp/b"),
                user_request: "Investigate lazy pagination cap".to_string(),
                created_at: now - Duration::from_secs(24 * 60 * 60),
                updated_at: now - Duration::from_secs(35 * 60),
                git_branch: Some("feature/resume".to_string()),
            },
            ResumePickerRow {
                project_path: PathBuf::from("/tmp/c"),
                user_request: "Explain the codebase".to_string(),
                created_at: now - Duration::from_secs(2 * 60 * 60),
                updated_at: now - Duration::from_secs(2 * 60 * 60),
                git_branch: Some("main".to_string()),
            },
        ];

        let mut screen = ResumePickerScreen::new(FrameRequester::test_dummy(), rows, now);
        screen.set_view_rows(5);
        screen.selected = 1;
        screen.ensure_selected_visible();

        screen.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        terminal
            .draw(|frame| {
                WidgetRef::render_ref(&&screen, frame.area(), frame.buffer_mut());
            })
            .expect("draw");

        assert_snapshot!(
            "resume_picker_table_created_sort_vt100",
            terminal.backend().vt100().screen().contents()
        );
    }
}
