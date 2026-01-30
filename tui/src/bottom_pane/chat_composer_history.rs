use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use serde::Serialize;

/// State machine that manages shell-style history navigation (Up/Down) inside
/// the chat composer. This struct is intentionally decoupled from the
/// rendering widget so the logic remains isolated and easier to test.
pub struct ChatComposerHistory {
    entries: Vec<HistoryEntry>,
    history_path: Option<PathBuf>,

    /// Current cursor within the history entries. `None` indicates the user is
    /// *not* currently browsing history.
    history_cursor: Option<isize>,

    /// The text that was last inserted into the composer as a result of
    /// history navigation. Used to decide if further Up/Down presses should be
    /// treated as navigation versus normal cursor movement.
    last_history_text: Option<String>,
}

impl ChatComposerHistory {
    pub fn new() -> Self {
        let history_path = resolve_history_path();
        let (entries, needs_truncate) = history_path
            .as_deref()
            .map(load_history_entries)
            .unwrap_or_default();
        if needs_truncate && let Some(path) = history_path.as_deref() {
            let _ = persist_history(path, &entries);
        }
        Self {
            entries,
            history_path,
            history_cursor: None,
            last_history_text: None,
        }
    }

    #[cfg(test)]
    pub fn new_with_history_path(history_path: PathBuf) -> Self {
        let (entries, _) = load_history_entries(&history_path);
        Self {
            entries,
            history_path: Some(history_path),
            history_cursor: None,
            last_history_text: None,
        }
    }

    /// Record a message submitted by the user in the current session so it can
    /// be recalled later.
    pub fn record_local_submission(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        self.history_cursor = None;
        self.last_history_text = None;

        // Avoid inserting a duplicate if identical to the previous entry.
        if self
            .entries
            .last()
            .is_some_and(|prev| prev.text.as_str() == text)
        {
            return;
        }

        self.entries.push(HistoryEntry {
            ts: unix_timestamp_secs(),
            text: text.to_string(),
        });
        if self.entries.len() > MAX_ENTRIES {
            let drop_count = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(0..drop_count);
        }

        if let Some(path) = self.history_path.as_deref()
            && let Err(err) = persist_history(path, &self.entries)
        {
            tracing::warn!(
                "failed to persist prompt history to {}: {err}",
                path.display()
            );
        }
    }

    /// Reset navigation tracking so the next Up key resumes from the latest entry.
    pub fn reset_navigation(&mut self) {
        self.history_cursor = None;
        self.last_history_text = None;
    }

    /// Should Up/Down key presses be interpreted as history navigation given
    /// the current content and cursor position of `textarea`?
    pub fn should_handle_navigation(&self, text: &str, cursor: usize) -> bool {
        if self.entries.is_empty() {
            return false;
        }

        if text.is_empty() {
            return true;
        }

        // Textarea is not empty – only navigate when cursor is at start and
        // text matches last recalled history entry so regular editing is not
        // hijacked.
        if cursor != 0 {
            return false;
        }

        matches!(&self.last_history_text, Some(prev) if prev == text)
    }

    /// Handle <Up>. Returns true when the key was consumed and the caller
    /// should request a redraw.
    pub fn navigate_up(&mut self, current_text: &str) -> Option<String> {
        let total_entries = self.entries.len();
        if total_entries == 0 {
            return None;
        }

        let mut next_idx = match self.history_cursor {
            None => (total_entries as isize) - 1,
            Some(0) => return None, // already at oldest
            Some(idx) => idx - 1,
        };

        while next_idx >= 0 {
            let entry = self.entries.get(next_idx as usize)?;
            if entry.text != current_text {
                self.history_cursor = Some(next_idx);
                self.last_history_text = Some(entry.text.clone());
                return Some(entry.text.clone());
            }
            if next_idx == 0 {
                break;
            }
            next_idx -= 1;
        }

        None
    }

    /// Handle <Down>.
    pub fn navigate_down(&mut self, current_text: &str) -> Option<String> {
        let total_entries = self.entries.len();
        if total_entries == 0 {
            return None;
        }

        let mut next_idx = match self.history_cursor {
            None => return None, // not browsing
            Some(idx) if (idx as usize) + 1 >= total_entries => {
                // Past newest – clear and exit browsing mode.
                self.history_cursor = None;
                self.last_history_text = None;
                return Some(String::new());
            }
            Some(idx) => idx + 1,
        };

        while (next_idx as usize) < total_entries {
            let entry = self.entries.get(next_idx as usize)?;
            if entry.text != current_text {
                self.history_cursor = Some(next_idx);
                self.last_history_text = Some(entry.text.clone());
                return Some(entry.text.clone());
            }
            next_idx += 1;
        }

        self.history_cursor = None;
        self.last_history_text = None;
        Some(String::new())
    }
}

const MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryEntry {
    ts: u64,
    text: String,
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn resolve_history_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        None
    }

    #[cfg(not(test))]
    {
        let home = dirs::home_dir()?;
        Some(home.join(".codexpotter").join("history.jsonl"))
    }
}

fn load_history_entries(path: &Path) -> (Vec<HistoryEntry>, bool) {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return (Vec::new(), false),
        Err(err) => {
            tracing::warn!(
                "failed to read prompt history from {}: {err}",
                path.display()
            );
            return (Vec::new(), false);
        }
    };

    let mut out = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: HistoryEntry = match serde_json::from_str(line) {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if entry.text.is_empty() {
            continue;
        }
        out.push(entry);
    }

    if out.len() <= MAX_ENTRIES {
        return (out, false);
    }

    let start = out.len() - MAX_ENTRIES;
    (out.split_off(start), true)
}

fn persist_history(path: &Path, entries: &[HistoryEntry]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("history path has no parent directory"))?;
    std::fs::create_dir_all(parent)?;

    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    for entry in entries {
        let line = serde_json::to_string(entry).map_err(|err| io::Error::other(err.to_string()))?;
        writeln!(tmp, "{line}")?;
    }
    tmp.flush()?;

    match tmp.persist(path) {
        Ok(_) => Ok(()),
        Err(err) => {
            // On platforms where rename won't overwrite, remove and try again.
            let _ = std::fs::remove_file(path);
            err.file.persist(path).map(|_| ()).map_err(|err| err.error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_submissions_are_not_recorded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("history.jsonl");
        let mut history = ChatComposerHistory::new_with_history_path(path);

        // Empty submissions are ignored.
        history.record_local_submission("");
        assert!(history.entries.is_empty());

        // First entry is recorded.
        history.record_local_submission("hello");
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.entries.last().unwrap().text, "hello");

        // Identical consecutive entry is skipped.
        history.record_local_submission("hello");
        assert_eq!(history.entries.len(), 1);

        // Different entry is recorded.
        history.record_local_submission("world");
        assert_eq!(history.entries.len(), 2);
        assert_eq!(history.entries.last().unwrap().text, "world");
    }

    #[test]
    fn navigation_skips_current_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("history.jsonl");
        let mut history = ChatComposerHistory::new_with_history_path(path);

        history.record_local_submission("same");
        history.record_local_submission("newer");
        history.record_local_submission("same");

        assert!(history.should_handle_navigation("", 0));
        assert_eq!(history.navigate_up(""), Some("same".to_string()));

        // When current text already matches the latest history entry, Up should skip it.
        assert!(history.should_handle_navigation("same", 0));
        assert_eq!(history.navigate_up("same"), Some("newer".to_string()));
    }

    #[test]
    fn persists_and_truncates_to_max_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("history.jsonl");

        {
            let mut history = ChatComposerHistory::new_with_history_path(path.clone());
            for idx in 0..(MAX_ENTRIES + 10) {
                history.record_local_submission(&format!("cmd {idx}"));
            }
        }

        let contents = std::fs::read_to_string(&path).expect("read history");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), MAX_ENTRIES);

        // Newer entries should be preserved.
        let last: HistoryEntry = serde_json::from_str(lines.last().unwrap()).expect("decode json");
        assert_eq!(last.text, format!("cmd {}", MAX_ENTRIES + 9));

        // Oldest entries should have been dropped.
        let first: HistoryEntry =
            serde_json::from_str(lines.first().unwrap()).expect("decode json");
        assert_eq!(first.text, "cmd 10");

        // Reloading should show the persisted latest entry.
        let mut history = ChatComposerHistory::new_with_history_path(path);
        assert_eq!(
            history.navigate_up(""),
            Some(format!("cmd {}", MAX_ENTRIES + 9))
        );
    }
}
