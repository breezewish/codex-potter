//! Persistent prompt history store.
//!
//! # Divergence from upstream Codex TUI
//!
//! Upstream Codex manages prompt history in the core/session layer and serves it via protocol
//! messages. `codex-potter` keeps a simple text-only history log under
//! `~/.codexpotter/history.jsonl` and serves `Op::GetHistoryEntryRequest` directly from the
//! render-only runner. See `tui/AGENTS.md`.

use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use serde::Serialize;

const MAX_ENTRIES: usize = 500;

/// Persistent prompt history backed by a JSONL file.
///
/// This is a deliberately small, text-only log used by CodexPotter's render-only runner. See the
/// module docs for how this differs from upstream Codex.
pub struct PromptHistoryStore {
    path: Option<PathBuf>,
    entries: Vec<HistoryEntry>,
    log_id: u64,
}

impl PromptHistoryStore {
    /// Create a store that loads from the default history path (if any).
    pub fn new() -> Self {
        Self::new_with_path(resolve_history_path())
    }

    /// Create a store backed by the provided history file path.
    ///
    /// This is primarily used by tests. When `path` is `None`, persistence is disabled and the
    /// store is purely in-memory.
    pub fn new_with_path(path: Option<PathBuf>) -> Self {
        let mut entries: Vec<HistoryEntry> = Vec::new();
        let mut log_id = 0u64;
        if let Some(path) = path.as_deref() {
            let (loaded, needs_truncate) = load_history_entries(path);
            entries = loaded;
            log_id = history_log_id_for_path(path);

            if needs_truncate {
                // Best-effort truncate so startup doesn't carry unbounded history.
                let _ = persist_history(path, &entries);
                log_id = history_log_id_for_path(path);
            }
        }

        Self {
            path,
            entries,
            log_id,
        }
    }

    /// Return the current history metadata as `(log_id, entry_count)`.
    ///
    /// `log_id` is derived from the backing file metadata (inode on Unix). Callers can use this to
    /// detect when the history file was replaced between requests.
    pub fn metadata(&self) -> (u64, usize) {
        (self.log_id, self.entries.len())
    }

    /// Look up a history entry by `(log_id, offset)` and return its text.
    ///
    /// Returns `None` when `log_id` does not match the current store (history file changed) or
    /// when the `offset` is out of bounds.
    pub fn lookup_text(&self, log_id: u64, offset: usize) -> Option<String> {
        if self.log_id != log_id {
            return None;
        }
        self.entries.get(offset).map(|entry| entry.text.clone())
    }

    /// Record a prompt submission (trimmed), dedupe consecutive duplicates, and persist
    /// best-effort.
    pub fn record_submission(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }

        if self.entries.last().is_some_and(|prev| prev.text == text) {
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

        let Some(path) = self.path.as_deref() else {
            return;
        };

        if let Err(err) = persist_history(path, &self.entries) {
            tracing::warn!(
                "failed to persist prompt history to {}: {err}",
                path.display()
            );
            return;
        }

        self.log_id = history_log_id_for_path(path);
    }
}

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

    let mut file = std::fs::File::create(path)?;
    for entry in entries {
        let line = serde_json::to_string(entry).map_err(|err| io::Error::other(err.to_string()))?;
        writeln!(file, "{line}")?;
    }
    file.flush()?;
    Ok(())
}

fn history_log_id_for_path(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::metadata(path) else {
        return 0;
    };

    history_log_id(&metadata).unwrap_or(0)
}

#[cfg(unix)]
fn history_log_id(metadata: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;

    Some(metadata.ino())
}

#[cfg(not(unix))]
fn history_log_id(_metadata: &std::fs::Metadata) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn record_submission_dedupes_consecutive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("history.jsonl");
        let mut store = PromptHistoryStore::new_with_path(Some(path));

        store.record_submission("");
        store.record_submission("hello");
        store.record_submission("hello");
        store.record_submission("world");

        assert_eq!(store.entries.len(), 2);
        assert_eq!(store.entries[0].text, "hello");
        assert_eq!(store.entries[1].text, "world");
    }

    #[test]
    fn persists_and_truncates_to_max_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("history.jsonl");

        let mut store = PromptHistoryStore::new_with_path(Some(path.clone()));
        for idx in 0..(MAX_ENTRIES + 10) {
            store.record_submission(&format!("cmd {idx}"));
        }

        let contents = std::fs::read_to_string(&path).expect("read history");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), MAX_ENTRIES);

        let first: HistoryEntry =
            serde_json::from_str(lines.first().unwrap()).expect("decode json");
        assert_eq!(first.text, "cmd 10");

        let last: HistoryEntry = serde_json::from_str(lines.last().unwrap()).expect("decode json");
        assert_eq!(last.text, format!("cmd {}", MAX_ENTRIES + 9));
    }
}
