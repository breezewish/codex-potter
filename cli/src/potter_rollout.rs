use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use codex_protocol::ThreadId;
use codex_protocol::protocol::PotterRoundOutcome;
use serde::Deserialize;
use serde::Serialize;

/// Name of the JSONL file that records CodexPotter session/round boundaries.
pub const POTTER_ROLLOUT_FILENAME: &str = "potter-rollout.jsonl";

/// A single append-only JSONL entry in `potter-rollout.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PotterRolloutLine {
    SessionStarted {
        user_message: Option<String>,
        user_prompt_file: PathBuf,
    },
    RoundStarted {
        current: u32,
        total: u32,
    },
    RoundConfigured {
        thread_id: ThreadId,
        rollout_path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rollout_path_raw: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rollout_base_dir: Option<PathBuf>,
    },
    SessionSucceeded {
        rounds: u32,
        duration_secs: u64,
        user_prompt_file: PathBuf,
        git_commit_start: String,
        git_commit_end: String,
    },
    RoundFinished {
        outcome: PotterRoundOutcome,
    },
}

/// Resolve the full path to `potter-rollout.jsonl` within a project directory.
pub fn potter_rollout_path(project_dir: &Path) -> PathBuf {
    project_dir.join(POTTER_ROLLOUT_FILENAME)
}

/// Append one JSON object + newline to the given JSONL file.
///
/// This is intentionally strict: failures are returned to the caller so the control plane can
/// abort rather than silently diverging from the persisted replay source of truth.
pub fn append_line(path: &Path, line: &PotterRolloutLine) -> anyhow::Result<()> {
    let Some(parent) = path.parent() else {
        anyhow::bail!(
            "invalid potter-rollout path (no parent): {}",
            path.display()
        );
    };
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;

    let mut json = serde_json::to_string(line)
        .with_context(|| format!("serialize potter-rollout line for {}", path.display()))?;
    json.push('\n');

    file.write_all(json.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    file.flush()
        .with_context(|| format!("flush {}", path.display()))?;
    Ok(())
}

/// Read and parse the entire `potter-rollout.jsonl` file.
pub fn read_lines(path: &Path) -> anyhow::Result<Vec<PotterRolloutLine>> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = std::io::BufReader::new(file);

    let mut out = Vec::new();
    for (idx, line) in std::io::BufRead::lines(reader).enumerate() {
        let line_number = idx + 1;
        let line =
            line.with_context(|| format!("read line {line_number} from {}", path.display()))?;
        if line.trim().is_empty() {
            anyhow::bail!("empty JSONL line {line_number} in {}", path.display());
        }
        let parsed = serde_json::from_str::<PotterRolloutLine>(&line)
            .with_context(|| format!("parse potter-rollout JSONL line {line_number}: {line}"))?;
        out.push(parsed);
    }

    Ok(out)
}

/// Best-effort: resolve a rollout path to an absolute path suitable for recording.
///
/// Returns:
/// - `rollout_path`: the absolute path (canonicalized when possible)
/// - `rollout_path_raw` + `rollout_base_dir`: only when canonicalization fails, for debugging.
pub fn resolve_rollout_path_for_recording(
    rollout_path: PathBuf,
    base_dir: &Path,
) -> (PathBuf, Option<PathBuf>, Option<PathBuf>) {
    let resolved = if rollout_path.is_absolute() {
        rollout_path.clone()
    } else {
        base_dir.join(&rollout_path)
    };

    match std::fs::canonicalize(&resolved) {
        Ok(canonical) => (canonical, None, None),
        Err(_) => (resolved, Some(rollout_path), Some(base_dir.to_path_buf())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn append_and_read_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("potter-rollout.jsonl");

        append_line(
            &log_path,
            &PotterRolloutLine::SessionStarted {
                user_message: Some("hello".to_string()),
                user_prompt_file: PathBuf::from(".codexpotter/projects/2026/02/28/1/MAIN.md"),
            },
        )
        .expect("append session_started");
        append_line(
            &log_path,
            &PotterRolloutLine::RoundStarted {
                current: 1,
                total: 10,
            },
        )
        .expect("append round_started");

        let lines = read_lines(&log_path).expect("read lines");
        assert_eq!(
            lines,
            vec![
                PotterRolloutLine::SessionStarted {
                    user_message: Some("hello".to_string()),
                    user_prompt_file: PathBuf::from(".codexpotter/projects/2026/02/28/1/MAIN.md"),
                },
                PotterRolloutLine::RoundStarted {
                    current: 1,
                    total: 10,
                },
            ]
        );
    }

    #[test]
    fn resolve_rollout_path_for_recording_canonicalizes_when_possible() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rollout_path = dir.path().join("rollout.jsonl");
        std::fs::write(&rollout_path, "[]").expect("write rollout");

        let (resolved, raw, base) =
            resolve_rollout_path_for_recording(PathBuf::from("rollout.jsonl"), dir.path());
        assert_eq!(resolved, rollout_path.canonicalize().expect("canonical"));
        assert_eq!(raw, None);
        assert_eq!(base, None);
    }

    #[test]
    fn resolve_rollout_path_for_recording_keeps_raw_when_canonicalize_fails() {
        let dir = tempfile::tempdir().expect("tempdir");

        let (resolved, raw, base) =
            resolve_rollout_path_for_recording(PathBuf::from("missing.jsonl"), dir.path());
        assert_eq!(resolved, dir.path().join("missing.jsonl"));
        assert_eq!(raw, Some(PathBuf::from("missing.jsonl")));
        assert_eq!(base, Some(dir.path().to_path_buf()));
    }
}
