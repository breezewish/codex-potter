use std::ffi::OsStr;
use std::io::BufRead as _;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::PotterRoundOutcome;
use codex_protocol::protocol::SessionConfiguredEvent;
use codex_tui::ExitReason;
use tokio::sync::mpsc::unbounded_channel;

const PROJECT_MAIN_FILE: &str = "MAIN.md";
const CODEXPOTTER_DIR: &str = ".codexpotter";

#[derive(Debug, Clone, PartialEq, Eq)]
/// Canonicalized paths derived from a user-provided `PROJECT_PATH`.
pub struct ResolvedProjectPaths {
    pub progress_file: PathBuf,
    pub project_dir: PathBuf,
    pub workdir: PathBuf,
}

/// Resolve a user-supplied project path into a unique `MAIN.md` progress file, plus derived dirs.
///
/// Supported input forms include:
/// - `2026/02/01/1`
/// - `.codexpotter/projects/2026/02/01/1`
/// - `/abs/path/to/.codexpotter/projects/2026/02/01/1`
/// - any of the above with `/MAIN.md` suffix
pub fn resolve_project_paths(
    cwd: &Path,
    project_path: &Path,
) -> anyhow::Result<ResolvedProjectPaths> {
    let project_path = crate::path_utils::expand_tilde(project_path);
    let candidates = build_candidate_progress_files(cwd, &project_path);

    let mut found: Vec<PathBuf> = Vec::new();
    let mut tried: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        tried.push(candidate.clone());
        if candidate.is_file() {
            let canonical = candidate
                .canonicalize()
                .with_context(|| format!("canonicalize {}", candidate.display()))?;
            if !found.contains(&canonical) {
                found.push(canonical);
            }
        }
    }

    let progress_file = match found.len() {
        0 => {
            let tried = tried
                .into_iter()
                .map(|path| format!("- {}", crate::path_utils::display_with_tilde(&path)))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!("no progress file found for project path. tried:\n{tried}");
        }
        1 => found.pop().context("pop single resolved progress file")?,
        _ => {
            let candidates = found
                .into_iter()
                .map(|path| format!("- {}", crate::path_utils::display_with_tilde(&path)))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!("ambiguous project path. candidates:\n{candidates}");
        }
    };

    let project_dir = progress_file
        .parent()
        .context("derive project_dir from progress_file")?
        .to_path_buf();

    let workdir = derive_project_workdir(&progress_file)?;

    Ok(ResolvedProjectPaths {
        progress_file,
        project_dir,
        workdir,
    })
}

pub async fn run_resume(
    ui: &mut codex_tui::CodexPotterTui,
    cwd: &Path,
    project_path: &Path,
) -> anyhow::Result<()> {
    let resolved = resolve_project_paths(cwd, project_path)?;
    let potter_rollout_path = crate::potter_rollout::potter_rollout_path(&resolved.project_dir);
    let potter_rollout_lines = crate::potter_rollout::read_lines(&potter_rollout_path)
        .with_context(|| format!("read {}", potter_rollout_path.display()))?;

    let round_plans = build_round_replay_plans(&resolved, &potter_rollout_lines)?;

    let (op_tx, mut op_rx) = unbounded_channel::<codex_protocol::protocol::Op>();
    tokio::spawn(async move { while op_rx.recv().await.is_some() {} });

    ui.clear().context("clear TUI before resume replay")?;

    for (idx, plan) in round_plans.into_iter().enumerate() {
        let RoundReplayPlan { events, outcome } = plan;
        let (event_tx, event_rx) = unbounded_channel::<Event>();
        for msg in events {
            let _ = event_tx.send(Event {
                id: "".to_string(),
                msg,
            });
        }
        drop(event_tx);

        let (_fatal_exit_tx, fatal_exit_rx) = unbounded_channel::<String>();

        let exit_info = ui
            .render_turn(
                String::new(),
                idx != 0,
                op_tx.clone(),
                event_rx,
                fatal_exit_rx,
            )
            .await?;

        match &exit_info.exit_reason {
            ExitReason::Fatal(message) => {
                anyhow::bail!("resume replay exited fatally: {message}");
            }
            ExitReason::UserRequested => {
                if !matches!(outcome, PotterRoundOutcome::UserRequested) {
                    break;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn build_candidate_progress_files(cwd: &Path, project_path: &Path) -> Vec<PathBuf> {
    if project_path.is_absolute() {
        return vec![ensure_main_md(project_path.to_path_buf())];
    }

    let a = cwd
        .join(CODEXPOTTER_DIR)
        .join("projects")
        .join(project_path);
    let b = cwd.join(project_path);

    vec![ensure_main_md(a), ensure_main_md(b)]
}

fn ensure_main_md(path: PathBuf) -> PathBuf {
    let is_main_md = path.file_name() == Some(OsStr::new(PROJECT_MAIN_FILE));
    if is_main_md {
        return path;
    }
    path.join(PROJECT_MAIN_FILE)
}

fn derive_project_workdir(progress_file: &Path) -> anyhow::Result<PathBuf> {
    let mut current = progress_file
        .parent()
        .context("progress file has no parent directory")?;

    loop {
        if current.file_name() == Some(OsStr::new(CODEXPOTTER_DIR)) {
            return current
                .parent()
                .context("derive project workdir from .codexpotter parent")?
                .to_path_buf()
                .canonicalize()
                .context("canonicalize project workdir");
        }

        current = current.parent().with_context(|| {
            format!(
                "progress file is not inside a `{CODEXPOTTER_DIR}` directory: {}",
                progress_file.display()
            )
        })?;
    }
}

struct RoundReplayPlan {
    events: Vec<EventMsg>,
    outcome: PotterRoundOutcome,
}

fn build_round_replay_plans(
    project: &ResolvedProjectPaths,
    potter_rollout_lines: &[crate::potter_rollout::PotterRolloutLine],
) -> anyhow::Result<Vec<RoundReplayPlan>> {
    let mut session_started: Option<(Option<String>, PathBuf)> = None;
    let mut rounds = Vec::new();

    struct RoundBuilder {
        started: (u32, u32),
        configured: Option<(codex_protocol::ThreadId, PathBuf)>,
        session_succeeded: Option<crate::potter_rollout::PotterRolloutLine>,
    }

    let mut current: Option<RoundBuilder> = None;

    for line in potter_rollout_lines {
        match line {
            crate::potter_rollout::PotterRolloutLine::SessionStarted {
                user_message,
                user_prompt_file,
            } => {
                if session_started.is_some() || !rounds.is_empty() || current.is_some() {
                    anyhow::bail!("potter-rollout: session_started must appear once at the top");
                }
                session_started = Some((user_message.clone(), user_prompt_file.clone()));
            }
            crate::potter_rollout::PotterRolloutLine::RoundStarted {
                current: round_current,
                total,
            } => {
                if current.is_some() {
                    anyhow::bail!("potter-rollout: round_started before previous round_finished");
                }
                current = Some(RoundBuilder {
                    started: (*round_current, *total),
                    configured: None,
                    session_succeeded: None,
                });
            }
            crate::potter_rollout::PotterRolloutLine::RoundConfigured {
                thread_id,
                rollout_path,
                ..
            } => {
                let Some(builder) = current.as_mut() else {
                    anyhow::bail!("potter-rollout: round_configured before round_started");
                };
                if builder.configured.is_some() {
                    anyhow::bail!("potter-rollout: duplicate round_configured in a single round");
                }
                builder.configured = Some((*thread_id, rollout_path.clone()));
            }
            crate::potter_rollout::PotterRolloutLine::SessionSucceeded { .. } => {
                let Some(builder) = current.as_mut() else {
                    anyhow::bail!("potter-rollout: session_succeeded outside a round");
                };
                if builder.session_succeeded.is_some() {
                    anyhow::bail!("potter-rollout: duplicate session_succeeded in a single round");
                }
                builder.session_succeeded = Some(line.clone());
            }
            crate::potter_rollout::PotterRolloutLine::RoundFinished { outcome } => {
                let Some(builder) = current.take() else {
                    anyhow::bail!("potter-rollout: round_finished without round_started");
                };
                let Some((thread_id, rollout_path)) = builder.configured else {
                    anyhow::bail!("potter-rollout: round_finished without round_configured");
                };

                let mut events = Vec::new();
                if rounds.is_empty() {
                    let Some((user_message, user_prompt_file)) = session_started.take() else {
                        anyhow::bail!("potter-rollout: missing session_started before first round");
                    };
                    events.push(EventMsg::PotterSessionStarted {
                        user_message,
                        working_dir: project.workdir.clone(),
                        project_dir: project.project_dir.clone(),
                        user_prompt_file,
                    });
                }

                events.push(EventMsg::PotterRoundStarted {
                    current: builder.started.0,
                    total: builder.started.1,
                });

                let rollout_path = resolve_rollout_path_for_replay(project, &rollout_path);
                if let Some(cfg) =
                    synthesize_session_configured_event(thread_id, rollout_path.clone())?
                {
                    events.push(EventMsg::SessionConfigured(cfg));
                }

                let mut rollout_events = read_upstream_rollout_event_msgs(&rollout_path)
                    .with_context(|| format!("replay rollout {}", rollout_path.display()))?;
                events.append(&mut rollout_events);

                if let Some(crate::potter_rollout::PotterRolloutLine::SessionSucceeded {
                    rounds,
                    duration_secs,
                    user_prompt_file,
                    git_commit_start,
                    git_commit_end,
                }) = builder.session_succeeded
                {
                    events.push(EventMsg::PotterSessionSucceeded {
                        rounds,
                        duration: std::time::Duration::from_secs(duration_secs),
                        user_prompt_file,
                        git_commit_start,
                        git_commit_end,
                    });
                }

                events.push(EventMsg::PotterRoundFinished {
                    outcome: outcome.clone(),
                });

                rounds.push(RoundReplayPlan {
                    events,
                    outcome: outcome.clone(),
                });
            }
        }
    }

    if current.is_some() {
        anyhow::bail!("potter-rollout: missing round_finished at EOF");
    }
    if session_started.is_some() && rounds.is_empty() {
        anyhow::bail!("potter-rollout: session_started present but no rounds found");
    }

    Ok(rounds)
}

fn resolve_rollout_path_for_replay(project: &ResolvedProjectPaths, rollout_path: &Path) -> PathBuf {
    if rollout_path.is_absolute() {
        return rollout_path.to_path_buf();
    }
    project.workdir.join(rollout_path)
}

fn synthesize_session_configured_event(
    thread_id: codex_protocol::ThreadId,
    rollout_path: PathBuf,
) -> anyhow::Result<Option<SessionConfiguredEvent>> {
    let Some(snapshot) = read_rollout_context_snapshot(&rollout_path)? else {
        return Ok(None);
    };

    Ok(Some(SessionConfiguredEvent {
        session_id: thread_id,
        forked_from_id: None,
        model: snapshot.model,
        model_provider_id: snapshot.model_provider_id,
        cwd: snapshot.cwd,
        reasoning_effort: None,
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        rollout_path,
    }))
}

struct RolloutContextSnapshot {
    cwd: PathBuf,
    model: String,
    model_provider_id: String,
}

fn read_rollout_context_snapshot(
    rollout_path: &Path,
) -> anyhow::Result<Option<RolloutContextSnapshot>> {
    let file = std::fs::File::open(rollout_path)
        .with_context(|| format!("open rollout {}", rollout_path.display()))?;
    let reader = std::io::BufReader::new(file);

    let mut cwd: Option<PathBuf> = None;
    let mut model: Option<String> = None;
    let mut model_provider_id: Option<String> = None;

    for (idx, line) in reader.lines().enumerate() {
        let line_number = idx + 1;
        let line = line.with_context(|| format!("read rollout line {line_number}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(&line)
            .with_context(|| format!("parse rollout json line {line_number}: {line}"))?;
        let Some(item_type) = value.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };
        match item_type {
            "turn_context" => {
                if cwd.is_some() && model.is_some() {
                    continue;
                }
                let Some(payload) = value.get("payload") else {
                    continue;
                };
                if cwd.is_none()
                    && let Some(v) = payload.get("cwd")
                {
                    cwd = serde_json::from_value::<PathBuf>(v.clone()).ok();
                }
                if model.is_none() {
                    model = payload
                        .get("model")
                        .and_then(serde_json::Value::as_str)
                        .map(ToOwned::to_owned);
                }
            }
            "session_meta" => {
                if model_provider_id.is_some() {
                    continue;
                }
                let Some(payload) = value.get("payload") else {
                    continue;
                };
                model_provider_id = payload
                    .get("model_provider")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned);
            }
            _ => {}
        }

        if cwd.is_some() && model.is_some() && model_provider_id.is_some() {
            break;
        }
    }

    let Some(cwd) = cwd else {
        return Ok(None);
    };
    let Some(model) = model else {
        return Ok(None);
    };

    Ok(Some(RolloutContextSnapshot {
        cwd,
        model,
        model_provider_id: model_provider_id.unwrap_or_default(),
    }))
}

fn read_upstream_rollout_event_msgs(rollout_path: &Path) -> anyhow::Result<Vec<EventMsg>> {
    let file = std::fs::File::open(rollout_path)
        .with_context(|| format!("open rollout {}", rollout_path.display()))?;
    let reader = std::io::BufReader::new(file);

    let mut out = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line_number = idx + 1;
        let line = line.with_context(|| format!("read rollout line {line_number}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(&line)
            .with_context(|| format!("parse rollout json line {line_number}: {line}"))?;
        let Some(item_type) = value.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if item_type != "event_msg" {
            continue;
        }
        let payload = value
            .get("payload")
            .context("rollout event_msg missing payload")?;
        let msg = serde_json::from_value::<EventMsg>(payload.clone())
            .with_context(|| format!("decode EventMsg from rollout line {line_number}"))?;
        out.push(msg);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn write_main(root: &Path, rel: &str) -> PathBuf {
        let path = root.join(rel).join("MAIN.md");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "---\nstatus: open\n---\n").expect("write MAIN.md");
        path
    }

    #[test]
    fn resolve_project_paths_supports_relative_short_form() {
        let temp = tempfile::tempdir().expect("tempdir");
        let main = write_main(temp.path(), ".codexpotter/projects/2026/02/01/1");

        let resolved =
            resolve_project_paths(temp.path(), Path::new("2026/02/01/1")).expect("resolve");

        assert_eq!(
            resolved.progress_file,
            main.canonicalize().expect("canonical")
        );
        assert_eq!(
            resolved.project_dir,
            main.canonicalize()
                .expect("canonical")
                .parent()
                .expect("project_dir")
                .to_path_buf()
        );
        assert_eq!(
            resolved.workdir,
            temp.path().canonicalize().expect("canonical")
        );
    }

    #[test]
    fn resolve_project_paths_accepts_absolute_project_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let main = write_main(temp.path(), ".codexpotter/projects/2026/02/01/1");
        let project_dir = main.parent().expect("project dir");

        let resolved = resolve_project_paths(temp.path(), project_dir).expect("resolve");
        assert_eq!(
            resolved.progress_file,
            main.canonicalize().expect("canonical")
        );
    }

    #[test]
    fn resolve_project_paths_errors_when_ambiguous() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _a = write_main(temp.path(), ".codexpotter/projects/foo");
        let _b = write_main(temp.path(), "foo");

        let err = resolve_project_paths(temp.path(), Path::new("foo"))
            .expect_err("expected ambiguity error");
        let message = format!("{err:#}");
        assert!(
            message.contains("ambiguous project path"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn resolve_project_paths_lists_tried_paths_on_missing() {
        let temp = tempfile::tempdir().expect("tempdir");

        let err = resolve_project_paths(temp.path(), Path::new("missing"))
            .expect_err("expected missing error");
        let message = format!("{err:#}");
        assert!(
            message.contains("no progress file found"),
            "unexpected error: {message}"
        );
        assert!(message.contains(".codexpotter/projects/missing/MAIN.md"));
        assert!(message.contains("missing/MAIN.md"));
    }

    #[test]
    fn read_upstream_rollout_event_msgs_extracts_event_msg_items() {
        let temp = tempfile::tempdir().expect("tempdir");
        let rollout_path = temp.path().join("rollout.jsonl");
        std::fs::write(
            &rollout_path,
            r#"{"timestamp":"2026-02-28T00:00:00.000Z","type":"event_msg","payload":{"type":"agent_message","message":"hello"}}
{"timestamp":"2026-02-28T00:00:00.000Z","type":"turn_context","payload":{"cwd":"project","approval_policy":"never","sandbox_policy":{"type":"read_only"},"model":"test-model","summary":{"type":"auto"},"output_schema":null}}
"#,
        )
        .expect("write rollout");

        let events = read_upstream_rollout_event_msgs(&rollout_path).expect("read events");
        assert_eq!(events.len(), 1);
        let EventMsg::AgentMessage(ev) = &events[0] else {
            panic!("expected agent_message, got: {:?}", events[0]);
        };
        assert_eq!(ev.message, "hello");
    }
}
