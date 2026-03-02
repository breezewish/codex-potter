use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::BufRead as _;
use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

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
    codex_bin: String,
    backend_launch: crate::app_server_backend::AppServerLaunchConfig,
    codex_compat_home: Option<PathBuf>,
    iterate_rounds: NonZeroUsize,
) -> anyhow::Result<ResumeExit> {
    let resolved = resolve_project_paths(cwd, project_path)?;
    std::env::set_current_dir(&resolved.workdir)
        .with_context(|| format!("set current directory to {}", resolved.workdir.display()))?;

    let progress_file_rel = resolved
        .progress_file
        .strip_prefix(&resolved.workdir)
        .context("derive progress file relative path")?
        .to_path_buf();
    let potter_rollout_path = crate::potter_rollout::potter_rollout_path(&resolved.project_dir);
    let potter_rollout_lines = load_potter_rollout_lines(&potter_rollout_path)?;

    let ResumeReplayPlans {
        completed_rounds: replay_rounds,
        mut unfinished_round,
    } = build_round_replay_plans(&resolved, &potter_rollout_lines)?;
    let has_completed_rounds = !replay_rounds.is_empty();

    let (op_tx, mut op_rx) = unbounded_channel::<codex_protocol::protocol::Op>();
    tokio::spawn(async move { while op_rx.recv().await.is_some() {} });

    ui.clear().context("clear TUI before resume replay")?;

    let mut user_cancelled_replay = false;
    for (idx, plan) in replay_rounds.into_iter().enumerate() {
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

        match replay_round_exit_decision(&exit_info.exit_reason, &outcome) {
            ReplayRoundExitDecision::Continue => {}
            ReplayRoundExitDecision::UserCancelled => {
                user_cancelled_replay = true;
                break;
            }
            ReplayRoundExitDecision::FatalExitRequested => {
                return Ok(ResumeExit::FatalExitRequested);
            }
        }
    }

    if user_cancelled_replay {
        return Ok(ResumeExit::Completed);
    }

    if let Some(unfinished) = unfinished_round.as_mut() {
        let events = build_unfinished_round_pre_action_events(&resolved, unfinished);

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
                has_completed_rounds,
                op_tx.clone(),
                event_rx,
                fatal_exit_rx,
            )
            .await?;

        match exit_info.exit_reason {
            ExitReason::Completed | ExitReason::TaskFailed(_) => {}
            ExitReason::UserRequested => return Ok(ResumeExit::Completed),
            ExitReason::Fatal(_) => return Ok(ResumeExit::FatalExitRequested),
        }
    }

    let iterate_rounds_usize = iterate_rounds.get();
    let action = match &unfinished_round {
        Some(unfinished) => {
            let remaining_rounds = unfinished.remaining_rounds_including_current()?;
            let rounds_label = if remaining_rounds == 1 {
                "round"
            } else {
                "rounds"
            };
            format!(
                "Continue & iterate {} more {}",
                remaining_rounds, rounds_label
            )
        }
        None => {
            let rounds_label = if iterate_rounds_usize == 1 {
                "round"
            } else {
                "rounds"
            };
            format!("Iterate {} more {}", iterate_rounds_usize, rounds_label)
        }
    };

    let selection = ui.prompt_action_picker(vec![action]).await?;
    let Some(index) = selection else {
        return Ok(ResumeExit::Completed);
    };
    if index != 0 {
        return Ok(ResumeExit::Completed);
    }

    crate::project::set_progress_file_finite_incantatem(
        &resolved.workdir,
        &progress_file_rel,
        false,
    )
    .context("reset progress file finite_incantatem")?;

    let baseline_rounds = count_completed_rounds(&potter_rollout_lines);
    let baseline_rounds_u32 = u32::try_from(baseline_rounds).unwrap_or(u32::MAX);

    let developer_prompt = crate::project::render_developer_prompt(&progress_file_rel);
    let turn_prompt = crate::project::fixed_prompt().trim_end().to_string();
    let git_commit_start =
        crate::project::progress_file_git_commit_start(&resolved.workdir, &progress_file_rel)
            .context("read git_commit from progress file")?;

    let round_context = crate::round_runner::PotterRoundContext {
        codex_bin,
        developer_prompt,
        backend_launch,
        codex_compat_home,
        thread_cwd: Some(resolved.workdir.clone()),
        turn_prompt,
        workdir: resolved.workdir.clone(),
        progress_file_rel: progress_file_rel.clone(),
        user_prompt_file: progress_file_rel.clone(),
        git_commit_start,
        potter_rollout_path,
        project_started_at: Instant::now(),
    };

    match unfinished_round {
        Some(unfinished) => {
            let remaining_rounds = unfinished.remaining_rounds_including_current()?;
            let remaining_after_continue = remaining_rounds.saturating_sub(1);

            let mut replay_event_msgs = Vec::new();
            if let Some((user_message, user_prompt_file)) = unfinished.session_started {
                replay_event_msgs.push(EventMsg::PotterSessionStarted {
                    user_message,
                    working_dir: resolved.workdir.clone(),
                    project_dir: resolved.project_dir.clone(),
                    user_prompt_file,
                });
            }
            if let Some(cfg) = synthesize_session_configured_event(
                unfinished.thread_id,
                unfinished.rollout_path.clone(),
            )? {
                replay_event_msgs.push(EventMsg::SessionConfigured(cfg));
            }
            let mut rollout_events = read_upstream_rollout_event_msgs(&unfinished.rollout_path)
                .with_context(|| format!("replay rollout {}", unfinished.rollout_path.display()))?;
            replay_event_msgs.append(&mut rollout_events);

            let round_result = crate::round_runner::continue_potter_round(
                ui,
                &round_context,
                crate::round_runner::PotterContinueRoundOptions {
                    pad_before_first_cell: true,
                    round_current: unfinished.round_current,
                    round_total: unfinished.round_total,
                    session_succeeded_rounds: baseline_rounds_u32.saturating_add(1),
                    resume_thread_id: unfinished.thread_id,
                    replay_event_msgs,
                },
            )
            .await?;

            match &round_result.exit_reason {
                ExitReason::UserRequested => return Ok(ResumeExit::Completed),
                ExitReason::TaskFailed(_) => return Ok(ResumeExit::Completed),
                ExitReason::Fatal(_) => return Ok(ResumeExit::FatalExitRequested),
                ExitReason::Completed => {}
            }
            if round_result.stop_due_to_finite_incantatem {
                return Ok(ResumeExit::Completed);
            }

            for offset in 0..remaining_after_continue {
                let current_round = unfinished
                    .round_current
                    .saturating_add(u32::try_from(offset.saturating_add(1)).unwrap_or(u32::MAX));
                let session_succeeded_rounds = baseline_rounds_u32
                    .saturating_add(u32::try_from(offset.saturating_add(2)).unwrap_or(u32::MAX));
                let round_result = crate::round_runner::run_potter_round(
                    ui,
                    &round_context,
                    crate::round_runner::PotterRoundOptions {
                        pad_before_first_cell: true,
                        session_started: None,
                        round_current: current_round,
                        round_total: unfinished.round_total,
                        session_succeeded_rounds,
                    },
                )
                .await?;

                match &round_result.exit_reason {
                    ExitReason::UserRequested => break,
                    ExitReason::TaskFailed(_) => break,
                    ExitReason::Fatal(_) => return Ok(ResumeExit::FatalExitRequested),
                    ExitReason::Completed => {}
                }
                if round_result.stop_due_to_finite_incantatem {
                    break;
                }
            }
        }
        None => {
            let iterate_rounds_u32 = u32::try_from(iterate_rounds_usize).unwrap_or(u32::MAX);
            for offset in 0..iterate_rounds_usize {
                let current_round = u32::try_from(offset.saturating_add(1)).unwrap_or(u32::MAX);
                let session_succeeded_rounds = baseline_rounds_u32.saturating_add(current_round);
                let round_result = crate::round_runner::run_potter_round(
                    ui,
                    &round_context,
                    crate::round_runner::PotterRoundOptions {
                        pad_before_first_cell: true,
                        session_started: None,
                        round_current: current_round,
                        round_total: iterate_rounds_u32,
                        session_succeeded_rounds,
                    },
                )
                .await?;

                match &round_result.exit_reason {
                    ExitReason::UserRequested => break,
                    ExitReason::TaskFailed(_) => break,
                    ExitReason::Fatal(_) => return Ok(ResumeExit::FatalExitRequested),
                    ExitReason::Completed => {}
                }
                if round_result.stop_due_to_finite_incantatem {
                    break;
                }
            }
        }
    }

    Ok(ResumeExit::Completed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeExit {
    Completed,
    FatalExitRequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayRoundExitDecision {
    Continue,
    UserCancelled,
    FatalExitRequested,
}

fn load_potter_rollout_lines(
    potter_rollout_path: &Path,
) -> anyhow::Result<Vec<crate::potter_rollout::PotterRolloutLine>> {
    if !potter_rollout_path.exists() {
        anyhow::bail!(
            "unsupported project: the project is from an older version of CodexPotter (missing potter-rollout.jsonl)",
        );
    }
    if !potter_rollout_path.is_file() {
        anyhow::bail!(
            "unsupported project: expected a file at {}",
            potter_rollout_path.display()
        );
    }

    let lines = crate::potter_rollout::read_lines(potter_rollout_path)
        .with_context(|| format!("read {}", potter_rollout_path.display()))?;
    if lines.is_empty() {
        anyhow::bail!("potter-rollout is empty: {}", potter_rollout_path.display());
    }

    Ok(lines)
}

fn replay_round_exit_decision(
    exit_reason: &ExitReason,
    outcome: &PotterRoundOutcome,
) -> ReplayRoundExitDecision {
    match exit_reason {
        ExitReason::Completed => ReplayRoundExitDecision::Continue,
        ExitReason::TaskFailed(_) => ReplayRoundExitDecision::Continue,
        ExitReason::Fatal(_) => match outcome {
            PotterRoundOutcome::Fatal { .. } => ReplayRoundExitDecision::Continue,
            _ => ReplayRoundExitDecision::FatalExitRequested,
        },
        ExitReason::UserRequested => match outcome {
            PotterRoundOutcome::UserRequested => ReplayRoundExitDecision::Continue,
            _ => ReplayRoundExitDecision::UserCancelled,
        },
    }
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

#[derive(Debug)]
struct RoundReplayPlan {
    events: Vec<EventMsg>,
    outcome: PotterRoundOutcome,
}

#[derive(Debug)]
struct ResumeReplayPlans {
    completed_rounds: Vec<RoundReplayPlan>,
    unfinished_round: Option<UnfinishedRoundPlan>,
}

#[derive(Debug)]
struct UnfinishedRoundPlan {
    round_current: u32,
    round_total: u32,
    thread_id: codex_protocol::ThreadId,
    rollout_path: PathBuf,
    session_started: Option<(Option<String>, PathBuf)>,
}

impl UnfinishedRoundPlan {
    fn remaining_rounds_including_current(&self) -> anyhow::Result<usize> {
        if self.round_current == 0 {
            anyhow::bail!("potter-rollout: round_current must be >= 1");
        }
        if self.round_total == 0 {
            anyhow::bail!("potter-rollout: round_total must be >= 1");
        }
        if self.round_current > self.round_total {
            anyhow::bail!(
                "potter-rollout: round_current {} exceeds round_total {}",
                self.round_current,
                self.round_total
            );
        }

        Ok(usize::try_from(
            self.round_total
                .saturating_sub(self.round_current)
                .saturating_add(1),
        )
        .unwrap_or(usize::MAX))
    }
}

fn count_completed_rounds(lines: &[crate::potter_rollout::PotterRolloutLine]) -> usize {
    lines
        .iter()
        .filter(|line| {
            matches!(
                line,
                crate::potter_rollout::PotterRolloutLine::RoundFinished { .. }
            )
        })
        .count()
}

fn build_round_replay_plans(
    project: &ResolvedProjectPaths,
    potter_rollout_lines: &[crate::potter_rollout::PotterRolloutLine],
) -> anyhow::Result<ResumeReplayPlans> {
    let index = crate::potter_rollout_resume_index::build_resume_index(potter_rollout_lines)?;

    let mut session_started = Some(index.session_started);
    let mut rounds = Vec::new();

    for round in index.completed_rounds {
        let mut events = Vec::new();
        if rounds.is_empty() {
            let started = session_started
                .take()
                .context("potter-rollout: missing session_started before first round")?;
            events.push(EventMsg::PotterSessionStarted {
                user_message: started.user_message,
                working_dir: project.workdir.clone(),
                project_dir: project.project_dir.clone(),
                user_prompt_file: started.user_prompt_file,
            });
        }

        events.push(EventMsg::PotterRoundStarted {
            current: round.round_current,
            total: round.round_total,
        });

        let rollout_path = resolve_rollout_path_for_replay(project, &round.rollout_path);
        if let Some(cfg) =
            synthesize_session_configured_event(round.thread_id, rollout_path.clone())?
        {
            events.push(EventMsg::SessionConfigured(cfg));
        }

        let mut rollout_events = read_upstream_rollout_event_msgs(&rollout_path)
            .with_context(|| format!("replay rollout {}", rollout_path.display()))?;
        events.append(&mut rollout_events);

        if let Some(session_succeeded) = round.session_succeeded {
            events.push(EventMsg::PotterSessionSucceeded {
                rounds: session_succeeded.rounds,
                duration: std::time::Duration::from_secs(session_succeeded.duration_secs),
                user_prompt_file: session_succeeded.user_prompt_file,
                git_commit_start: session_succeeded.git_commit_start,
                git_commit_end: session_succeeded.git_commit_end,
            });
        }

        events.push(EventMsg::PotterRoundFinished {
            outcome: round.outcome.clone(),
        });

        rounds.push(RoundReplayPlan {
            events,
            outcome: round.outcome,
        });
    }

    let unfinished_round = index.unfinished_round.map(|round| UnfinishedRoundPlan {
        round_current: round.round_current,
        round_total: round.round_total,
        thread_id: round.thread_id,
        rollout_path: resolve_rollout_path_for_replay(project, &round.rollout_path),
        session_started: session_started
            .map(|started| (started.user_message, started.user_prompt_file)),
    });

    Ok(ResumeReplayPlans {
        completed_rounds: rounds,
        unfinished_round,
    })
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

    Ok(filter_pending_interactive_prompts_for_replay(out))
}

/// Build the minimal replay events needed to show an unfinished round boundary before prompting.
///
/// Note: the trailing `PotterRoundFinished` is synthesized so the render-only runner exits cleanly
/// (otherwise EOF would be treated as a fatal "Backend disconnected").
fn build_unfinished_round_pre_action_events(
    project: &ResolvedProjectPaths,
    unfinished: &mut UnfinishedRoundPlan,
) -> Vec<EventMsg> {
    let mut events = Vec::new();
    if let Some((user_message, user_prompt_file)) = unfinished.session_started.take() {
        events.push(EventMsg::PotterSessionStarted {
            user_message,
            working_dir: project.workdir.clone(),
            project_dir: project.project_dir.clone(),
            user_prompt_file,
        });
    }
    events.push(EventMsg::PotterRoundStarted {
        current: unfinished.round_current,
        total: unfinished.round_total,
    });
    events.push(EventMsg::PotterRoundFinished {
        outcome: PotterRoundOutcome::Completed,
    });
    events
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ElicitationRequestKey {
    server_name: String,
    request_id: codex_protocol::mcp::RequestId,
}

impl ElicitationRequestKey {
    fn new(server_name: String, request_id: codex_protocol::mcp::RequestId) -> Self {
        Self {
            server_name,
            request_id,
        }
    }
}

#[derive(Debug, Default)]
struct PendingInteractiveReplayState {
    exec_approval_call_ids: HashSet<String>,
    exec_approval_call_ids_by_turn_id: HashMap<String, Vec<String>>,
    patch_approval_call_ids: HashSet<String>,
    patch_approval_call_ids_by_turn_id: HashMap<String, Vec<String>>,
    elicitation_requests: HashSet<ElicitationRequestKey>,
    request_user_input_call_ids: HashSet<String>,
    request_user_input_call_ids_by_turn_id: HashMap<String, Vec<String>>,
}

impl PendingInteractiveReplayState {
    fn note_event_msg(&mut self, msg: &EventMsg) {
        match msg {
            EventMsg::ExecApprovalRequest(ev) => {
                let approval_id = ev.effective_approval_id();
                self.exec_approval_call_ids.insert(approval_id.clone());
                self.exec_approval_call_ids_by_turn_id
                    .entry(ev.turn_id.clone())
                    .or_default()
                    .push(approval_id);
            }
            EventMsg::ExecCommandBegin(ev) => {
                self.exec_approval_call_ids.remove(&ev.call_id);
                Self::remove_call_id_from_turn_map(
                    &mut self.exec_approval_call_ids_by_turn_id,
                    &ev.call_id,
                );
            }
            EventMsg::ApplyPatchApprovalRequest(ev) => {
                self.patch_approval_call_ids.insert(ev.call_id.clone());
                self.patch_approval_call_ids_by_turn_id
                    .entry(ev.turn_id.clone())
                    .or_default()
                    .push(ev.call_id.clone());
            }
            EventMsg::PatchApplyBegin(ev) => {
                self.patch_approval_call_ids.remove(&ev.call_id);
                Self::remove_call_id_from_turn_map(
                    &mut self.patch_approval_call_ids_by_turn_id,
                    &ev.call_id,
                );
            }
            EventMsg::ElicitationRequest(ev) => {
                self.elicitation_requests.insert(ElicitationRequestKey::new(
                    ev.server_name.clone(),
                    ev.id.clone(),
                ));
            }
            EventMsg::RequestUserInput(ev) => {
                self.request_user_input_call_ids.insert(ev.call_id.clone());
                self.request_user_input_call_ids_by_turn_id
                    .entry(ev.turn_id.clone())
                    .or_default()
                    .push(ev.call_id.clone());
            }
            EventMsg::TurnComplete(ev) => {
                self.clear_exec_approval_turn(&ev.turn_id);
                self.clear_patch_approval_turn(&ev.turn_id);
                self.clear_request_user_input_turn(&ev.turn_id);
            }
            EventMsg::TurnAborted(ev) => {
                if let Some(turn_id) = &ev.turn_id {
                    self.clear_exec_approval_turn(turn_id);
                    self.clear_patch_approval_turn(turn_id);
                    self.clear_request_user_input_turn(turn_id);
                }
            }
            EventMsg::ShutdownComplete => self.clear(),
            _ => {}
        }
    }

    fn should_replay_snapshot_event_msg(&self, msg: &EventMsg) -> bool {
        match msg {
            EventMsg::ExecApprovalRequest(ev) => self
                .exec_approval_call_ids
                .contains(&ev.effective_approval_id()),
            EventMsg::ApplyPatchApprovalRequest(ev) => {
                self.patch_approval_call_ids.contains(&ev.call_id)
            }
            EventMsg::ElicitationRequest(ev) => {
                self.elicitation_requests
                    .contains(&ElicitationRequestKey::new(
                        ev.server_name.clone(),
                        ev.id.clone(),
                    ))
            }
            EventMsg::RequestUserInput(ev) => {
                self.request_user_input_call_ids.contains(&ev.call_id)
            }
            _ => true,
        }
    }

    fn clear_request_user_input_turn(&mut self, turn_id: &str) {
        if let Some(call_ids) = self.request_user_input_call_ids_by_turn_id.remove(turn_id) {
            for call_id in call_ids {
                self.request_user_input_call_ids.remove(&call_id);
            }
        }
    }

    fn clear_exec_approval_turn(&mut self, turn_id: &str) {
        if let Some(call_ids) = self.exec_approval_call_ids_by_turn_id.remove(turn_id) {
            for call_id in call_ids {
                self.exec_approval_call_ids.remove(&call_id);
            }
        }
    }

    fn clear_patch_approval_turn(&mut self, turn_id: &str) {
        if let Some(call_ids) = self.patch_approval_call_ids_by_turn_id.remove(turn_id) {
            for call_id in call_ids {
                self.patch_approval_call_ids.remove(&call_id);
            }
        }
    }

    fn remove_call_id_from_turn_map(
        call_ids_by_turn_id: &mut HashMap<String, Vec<String>>,
        call_id: &str,
    ) {
        call_ids_by_turn_id.retain(|_, call_ids| {
            call_ids.retain(|queued_call_id| queued_call_id != call_id);
            !call_ids.is_empty()
        });
    }

    fn clear(&mut self) {
        self.exec_approval_call_ids.clear();
        self.exec_approval_call_ids_by_turn_id.clear();
        self.patch_approval_call_ids.clear();
        self.patch_approval_call_ids_by_turn_id.clear();
        self.elicitation_requests.clear();
        self.request_user_input_call_ids.clear();
        self.request_user_input_call_ids_by_turn_id.clear();
    }
}

fn filter_pending_interactive_prompts_for_replay(events: Vec<EventMsg>) -> Vec<EventMsg> {
    let mut state = PendingInteractiveReplayState::default();
    for msg in &events {
        state.note_event_msg(msg);
    }

    events
        .into_iter()
        .filter(|msg| state.should_replay_snapshot_event_msg(msg))
        .collect()
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

    #[test]
    fn read_upstream_rollout_event_msgs_filters_resolved_exec_approval_prompt() {
        let temp = tempfile::tempdir().expect("tempdir");
        let rollout_path = temp.path().join("rollout.jsonl");
        std::fs::write(
            &rollout_path,
            r#"{"timestamp":"2026-02-28T00:00:00.000Z","type":"event_msg","payload":{"type":"exec_approval_request","call_id":"call-1","turn_id":"turn-1","command":["echo","hi"],"cwd":"/tmp","parsed_cmd":[]}}
{"timestamp":"2026-02-28T00:00:01.000Z","type":"event_msg","payload":{"type":"exec_command_begin","call_id":"call-1","turn_id":"turn-1","command":["echo","hi"],"cwd":"/tmp","parsed_cmd":[]}}
"#,
        )
        .expect("write rollout");

        let events = read_upstream_rollout_event_msgs(&rollout_path).expect("read events");
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], EventMsg::ExecCommandBegin(ev) if ev.call_id == "call-1"),
            "unexpected events: {events:?}"
        );
    }

    #[test]
    fn read_upstream_rollout_event_msgs_keeps_pending_request_user_input() {
        let temp = tempfile::tempdir().expect("tempdir");
        let rollout_path = temp.path().join("rollout.jsonl");
        std::fs::write(
            &rollout_path,
            r#"{"timestamp":"2026-02-28T00:00:00.000Z","type":"event_msg","payload":{"type":"request_user_input","call_id":"call-1","turn_id":"turn-1","questions":[]}}
"#,
        )
        .expect("write rollout");

        let events = read_upstream_rollout_event_msgs(&rollout_path).expect("read events");
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], EventMsg::RequestUserInput(ev) if ev.call_id == "call-1"),
            "unexpected events: {events:?}"
        );
    }

    #[test]
    fn read_upstream_rollout_event_msgs_drops_resolved_request_user_input_after_turn_complete() {
        let temp = tempfile::tempdir().expect("tempdir");
        let rollout_path = temp.path().join("rollout.jsonl");
        std::fs::write(
            &rollout_path,
            r#"{"timestamp":"2026-02-28T00:00:00.000Z","type":"event_msg","payload":{"type":"request_user_input","call_id":"call-1","turn_id":"turn-1","questions":[]}}
{"timestamp":"2026-02-28T00:00:01.000Z","type":"event_msg","payload":{"type":"turn_complete","turn_id":"turn-1","last_agent_message":null}}
"#,
        )
        .expect("write rollout");

        let events = read_upstream_rollout_event_msgs(&rollout_path).expect("read events");
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], EventMsg::TurnComplete(ev) if ev.turn_id == "turn-1"),
            "unexpected events: {events:?}"
        );
    }

    #[test]
    fn replay_round_exit_decision_allows_historical_fatal_outcome() {
        let decision = replay_round_exit_decision(
            &ExitReason::Fatal("boom".to_string()),
            &PotterRoundOutcome::Fatal {
                message: "boom".to_string(),
            },
        );
        assert_eq!(decision, ReplayRoundExitDecision::Continue);
    }

    #[test]
    fn replay_round_exit_decision_treats_unexpected_fatal_as_fatal_exit() {
        let decision = replay_round_exit_decision(
            &ExitReason::Fatal("backend disconnected".to_string()),
            &PotterRoundOutcome::Completed,
        );
        assert_eq!(decision, ReplayRoundExitDecision::FatalExitRequested);
    }

    #[test]
    fn load_potter_rollout_lines_errors_when_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("potter-rollout.jsonl");

        let err = load_potter_rollout_lines(&path).expect_err("expected missing error");
        let message = format!("{err:#}");
        assert!(
            message.contains("the project is from an older version of CodexPotter"),
            "unexpected error: {message}"
        );
        assert!(message.contains("potter-rollout.jsonl"));
    }

    #[test]
    fn build_round_replay_plans_returns_unfinished_round_at_eof() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _main = write_main(temp.path(), ".codexpotter/projects/2026/02/01/1");
        let resolved =
            resolve_project_paths(temp.path(), Path::new("2026/02/01/1")).expect("resolve");

        let thread_id =
            codex_protocol::ThreadId::from_string("019ca423-63d9-7641-ae83-db060ad3c000")
                .expect("thread id");
        let potter_rollout_lines = vec![
            crate::potter_rollout::PotterRolloutLine::SessionStarted {
                user_message: Some("hello".to_string()),
                user_prompt_file: PathBuf::from(".codexpotter/projects/2026/02/01/1/MAIN.md"),
            },
            crate::potter_rollout::PotterRolloutLine::RoundStarted {
                current: 1,
                total: 10,
            },
            crate::potter_rollout::PotterRolloutLine::RoundConfigured {
                thread_id,
                rollout_path: PathBuf::from("rollout.jsonl"),
                rollout_path_raw: None,
                rollout_base_dir: None,
            },
        ];

        let plans =
            build_round_replay_plans(&resolved, &potter_rollout_lines).expect("build plans");
        assert_eq!(plans.completed_rounds.len(), 0);

        let unfinished = plans.unfinished_round.expect("unfinished round");
        assert_eq!(unfinished.round_current, 1);
        assert_eq!(unfinished.round_total, 10);
        assert_eq!(unfinished.thread_id, thread_id);
        assert_eq!(
            unfinished.rollout_path,
            resolved.workdir.join("rollout.jsonl")
        );
        assert_eq!(unfinished.remaining_rounds_including_current().unwrap(), 10);
        assert_eq!(
            unfinished.session_started,
            Some((
                Some("hello".to_string()),
                PathBuf::from(".codexpotter/projects/2026/02/01/1/MAIN.md"),
            ))
        );
    }

    #[test]
    fn build_round_replay_plans_consumes_session_started_in_first_completed_round() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _main = write_main(temp.path(), ".codexpotter/projects/2026/02/01/1");
        let resolved =
            resolve_project_paths(temp.path(), Path::new("2026/02/01/1")).expect("resolve");

        std::fs::write(resolved.workdir.join("first.jsonl"), "").expect("write first rollout");

        let thread_id =
            codex_protocol::ThreadId::from_string("019ca423-63d9-7641-ae83-db060ad3c000")
                .expect("thread id");
        let next_thread_id =
            codex_protocol::ThreadId::from_string("019ca42b-38d5-7be2-9d37-d223f40b8748")
                .expect("next thread id");

        let potter_rollout_lines = vec![
            crate::potter_rollout::PotterRolloutLine::SessionStarted {
                user_message: Some("hello".to_string()),
                user_prompt_file: PathBuf::from(".codexpotter/projects/2026/02/01/1/MAIN.md"),
            },
            crate::potter_rollout::PotterRolloutLine::RoundStarted {
                current: 1,
                total: 10,
            },
            crate::potter_rollout::PotterRolloutLine::RoundConfigured {
                thread_id,
                rollout_path: PathBuf::from("first.jsonl"),
                rollout_path_raw: None,
                rollout_base_dir: None,
            },
            crate::potter_rollout::PotterRolloutLine::RoundFinished {
                outcome: PotterRoundOutcome::Completed,
            },
            crate::potter_rollout::PotterRolloutLine::RoundStarted {
                current: 2,
                total: 10,
            },
            crate::potter_rollout::PotterRolloutLine::RoundConfigured {
                thread_id: next_thread_id,
                rollout_path: PathBuf::from("second.jsonl"),
                rollout_path_raw: None,
                rollout_base_dir: None,
            },
        ];

        let plans =
            build_round_replay_plans(&resolved, &potter_rollout_lines).expect("build plans");
        assert_eq!(plans.completed_rounds.len(), 1);

        let unfinished = plans.unfinished_round.expect("unfinished round");
        assert_eq!(unfinished.session_started, None);
    }

    #[test]
    fn build_round_replay_plans_errors_when_unfinished_round_is_missing_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _main = write_main(temp.path(), ".codexpotter/projects/2026/02/01/1");
        let resolved =
            resolve_project_paths(temp.path(), Path::new("2026/02/01/1")).expect("resolve");

        let potter_rollout_lines = vec![
            crate::potter_rollout::PotterRolloutLine::SessionStarted {
                user_message: Some("hello".to_string()),
                user_prompt_file: PathBuf::from(".codexpotter/projects/2026/02/01/1/MAIN.md"),
            },
            crate::potter_rollout::PotterRolloutLine::RoundStarted {
                current: 1,
                total: 10,
            },
        ];

        let err =
            build_round_replay_plans(&resolved, &potter_rollout_lines).expect_err("expected error");
        let message = format!("{err:#}");
        assert!(
            message.contains("missing round_configured"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn build_unfinished_round_pre_action_events_replays_session_started_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _main = write_main(temp.path(), ".codexpotter/projects/2026/02/01/1");
        let resolved =
            resolve_project_paths(temp.path(), Path::new("2026/02/01/1")).expect("resolve");

        let thread_id =
            codex_protocol::ThreadId::from_string("019ca423-63d9-7641-ae83-db060ad3c000")
                .expect("thread id");
        let mut unfinished = UnfinishedRoundPlan {
            round_current: 1,
            round_total: 10,
            thread_id,
            rollout_path: resolved.workdir.join("rollout.jsonl"),
            session_started: Some((
                Some("hello".to_string()),
                PathBuf::from(".codexpotter/projects/2026/02/01/1/MAIN.md"),
            )),
        };

        let events = build_unfinished_round_pre_action_events(&resolved, &mut unfinished);

        assert_eq!(unfinished.session_started, None);
        assert_eq!(events.len(), 3);
        let EventMsg::PotterSessionStarted {
            user_message,
            working_dir,
            project_dir,
            user_prompt_file,
        } = &events[0]
        else {
            panic!("expected PotterSessionStarted, got: {:?}", events[0]);
        };
        assert_eq!(user_message.as_deref(), Some("hello"));
        assert_eq!(working_dir, &resolved.workdir);
        assert_eq!(project_dir, &resolved.project_dir);
        assert_eq!(
            user_prompt_file,
            &PathBuf::from(".codexpotter/projects/2026/02/01/1/MAIN.md")
        );
        let EventMsg::PotterRoundStarted { current, total } = &events[1] else {
            panic!("expected PotterRoundStarted, got: {:?}", events[1]);
        };
        assert_eq!(*current, 1);
        assert_eq!(*total, 10);

        let EventMsg::PotterRoundFinished { outcome } = &events[2] else {
            panic!("expected PotterRoundFinished, got: {:?}", events[2]);
        };
        assert_eq!(*outcome, PotterRoundOutcome::Completed);
    }

    #[test]
    fn build_unfinished_round_pre_action_events_skips_when_session_started_already_consumed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _main = write_main(temp.path(), ".codexpotter/projects/2026/02/01/1");
        let resolved =
            resolve_project_paths(temp.path(), Path::new("2026/02/01/1")).expect("resolve");

        let thread_id =
            codex_protocol::ThreadId::from_string("019ca423-63d9-7641-ae83-db060ad3c000")
                .expect("thread id");
        let mut unfinished = UnfinishedRoundPlan {
            round_current: 2,
            round_total: 10,
            thread_id,
            rollout_path: resolved.workdir.join("rollout.jsonl"),
            session_started: None,
        };

        let events = build_unfinished_round_pre_action_events(&resolved, &mut unfinished);

        assert_eq!(unfinished.session_started, None);
        assert_eq!(events.len(), 2);

        let EventMsg::PotterRoundStarted { current, total } = &events[0] else {
            panic!("expected PotterRoundStarted, got: {:?}", events[0]);
        };
        assert_eq!(*current, 2);
        assert_eq!(*total, 10);

        let EventMsg::PotterRoundFinished { outcome } = &events[1] else {
            panic!("expected PotterRoundFinished, got: {:?}", events[1]);
        };
        assert_eq!(*outcome, PotterRoundOutcome::Completed);
    }
}
