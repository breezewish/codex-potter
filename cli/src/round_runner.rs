use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::PotterRoundOutcome;
use codex_tui::ExitReason;
use tokio::sync::mpsc::unbounded_channel;

#[derive(Debug, Clone)]
pub struct PotterRoundContext {
    pub codex_bin: String,
    pub developer_prompt: String,
    pub backend_launch: crate::app_server_backend::AppServerLaunchConfig,
    pub codex_compat_home: Option<PathBuf>,
    pub thread_cwd: Option<PathBuf>,
    pub turn_prompt: String,
    pub workdir: PathBuf,
    pub progress_file_rel: PathBuf,
    pub user_prompt_file: PathBuf,
    pub git_commit_start: String,
    pub potter_rollout_path: PathBuf,
    pub project_started_at: Instant,
}

#[derive(Debug, Clone)]
pub struct PotterSessionStartedInfo {
    pub user_message: Option<String>,
    pub working_dir: PathBuf,
    pub project_dir: PathBuf,
    pub user_prompt_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PotterRoundOptions {
    pub pad_before_first_cell: bool,
    pub session_started: Option<PotterSessionStartedInfo>,
    pub round_current: u32,
    pub round_total: u32,
    pub session_succeeded_rounds: u32,
}

#[derive(Debug, Clone)]
pub struct PotterContinueRoundOptions {
    /// Whether to pad the transcript with a blank line before the first rendered cell.
    pub pad_before_first_cell: bool,
    pub round_current: u32,
    pub round_total: u32,
    pub session_succeeded_rounds: u32,
    /// Existing Codex thread to resume for this unfinished round.
    pub resume_thread_id: codex_protocol::ThreadId,
    /// Persisted EventMsg items from the upstream rollout to replay before continuing.
    pub replay_event_msgs: Vec<EventMsg>,
}

#[derive(Debug)]
pub struct PotterRoundResult {
    pub exit_reason: ExitReason,
    pub stop_due_to_finite_incantatem: bool,
}

pub async fn run_potter_round(
    ui: &mut codex_tui::CodexPotterTui,
    context: &PotterRoundContext,
    options: PotterRoundOptions,
) -> anyhow::Result<PotterRoundResult> {
    let PotterRoundOptions {
        pad_before_first_cell,
        session_started,
        round_current,
        round_total,
        session_succeeded_rounds,
    } = options;

    run_potter_round_inner(
        ui,
        context,
        PotterRoundInnerOptions {
            pad_before_first_cell,
            session_started,
            round_current,
            round_total,
            session_succeeded_rounds,
            prompt: context.turn_prompt.clone(),
            resume_thread_id: None,
            record_round_started: true,
            record_round_configured: true,
            replay_event_msgs: Vec::new(),
        },
    )
    .await
}

/// Continue an unfinished round by resuming its thread and sending a `Continue` prompt.
///
/// This is primarily used by `codex-potter resume` when the last recorded round has no
/// `PotterRoundFinished` marker yet.
pub async fn continue_potter_round(
    ui: &mut codex_tui::CodexPotterTui,
    context: &PotterRoundContext,
    options: PotterContinueRoundOptions,
) -> anyhow::Result<PotterRoundResult> {
    let PotterContinueRoundOptions {
        pad_before_first_cell,
        round_current,
        round_total,
        session_succeeded_rounds,
        resume_thread_id,
        replay_event_msgs,
    } = options;

    run_potter_round_inner(
        ui,
        context,
        PotterRoundInnerOptions {
            pad_before_first_cell,
            session_started: None,
            round_current,
            round_total,
            session_succeeded_rounds,
            prompt: String::from("Continue"),
            resume_thread_id: Some(resume_thread_id),
            record_round_started: false,
            record_round_configured: false,
            replay_event_msgs,
        },
    )
    .await
}

struct PotterRoundInnerOptions {
    pad_before_first_cell: bool,
    session_started: Option<PotterSessionStartedInfo>,
    round_current: u32,
    round_total: u32,
    session_succeeded_rounds: u32,
    prompt: String,
    resume_thread_id: Option<codex_protocol::ThreadId>,
    record_round_started: bool,
    record_round_configured: bool,
    replay_event_msgs: Vec<EventMsg>,
}

async fn run_potter_round_inner(
    ui: &mut codex_tui::CodexPotterTui,
    context: &PotterRoundContext,
    options: PotterRoundInnerOptions,
) -> anyhow::Result<PotterRoundResult> {
    let PotterRoundInnerOptions {
        pad_before_first_cell,
        session_started,
        round_current,
        round_total,
        session_succeeded_rounds,
        prompt,
        resume_thread_id,
        record_round_started,
        record_round_configured,
        replay_event_msgs,
    } = options;

    let (op_tx, op_rx) = unbounded_channel::<Op>();
    let (backend_event_tx, mut backend_event_rx) = unbounded_channel::<Event>();
    let (ui_event_tx, ui_event_rx) = unbounded_channel::<Event>();
    let (fatal_exit_tx, fatal_exit_rx) = unbounded_channel::<String>();

    if let Some(session_started) = session_started {
        let _ = ui_event_tx.send(Event {
            id: "".to_string(),
            msg: EventMsg::PotterSessionStarted {
                user_message: session_started.user_message.clone(),
                working_dir: session_started.working_dir,
                project_dir: session_started.project_dir,
                user_prompt_file: session_started.user_prompt_file.clone(),
            },
        });
        crate::potter_rollout::append_line(
            &context.potter_rollout_path,
            &crate::potter_rollout::PotterRolloutLine::SessionStarted {
                user_message: session_started.user_message,
                user_prompt_file: session_started.user_prompt_file,
            },
        )
        .context("append potter-rollout session_started")?;
    }

    let _ = ui_event_tx.send(Event {
        id: "".to_string(),
        msg: EventMsg::PotterRoundStarted {
            current: round_current,
            total: round_total,
        },
    });
    if record_round_started {
        crate::potter_rollout::append_line(
            &context.potter_rollout_path,
            &crate::potter_rollout::PotterRolloutLine::RoundStarted {
                current: round_current,
                total: round_total,
            },
        )
        .context("append potter-rollout round_started")?;
    }

    for msg in replay_event_msgs {
        let _ = ui_event_tx.send(Event {
            id: "".to_string(),
            msg,
        });
    }

    let forwarder = {
        let ui_event_tx = ui_event_tx.clone();
        let workdir = context.workdir.clone();
        let progress_file_rel = context.progress_file_rel.clone();
        let user_prompt_file = context.user_prompt_file.clone();
        let git_commit_start = context.git_commit_start.clone();
        let potter_rollout_path = context.potter_rollout_path.clone();
        let fatal_exit_tx = fatal_exit_tx.clone();
        let project_started_at = context.project_started_at;

        tokio::spawn(async move {
            let mut has_recorded_round_configured = !record_round_configured;
            while let Some(event) = backend_event_rx.recv().await {
                if !has_recorded_round_configured
                    && let EventMsg::SessionConfigured(cfg) = &event.msg
                {
                    has_recorded_round_configured = true;
                    let (rollout_path, rollout_path_raw, rollout_base_dir) =
                        crate::potter_rollout::resolve_rollout_path_for_recording(
                            cfg.rollout_path.clone(),
                            &workdir,
                        );
                    if let Err(err) = crate::potter_rollout::append_line(
                        &potter_rollout_path,
                        &crate::potter_rollout::PotterRolloutLine::RoundConfigured {
                            thread_id: cfg.session_id,
                            rollout_path,
                            rollout_path_raw,
                            rollout_base_dir,
                        },
                    ) {
                        let _ = fatal_exit_tx.send(format!(
                            "failed to write {}: {err:#}",
                            potter_rollout_path.display()
                        ));
                        break;
                    }
                }

                if matches!(
                    &event.msg,
                    EventMsg::PotterRoundFinished {
                        outcome: PotterRoundOutcome::Completed
                    }
                ) && crate::project::progress_file_has_finite_incantatem_true(
                    &workdir,
                    &progress_file_rel,
                )
                .unwrap_or(false)
                {
                    if let Err(err) = crate::potter_rollout::append_line(
                        &potter_rollout_path,
                        &crate::potter_rollout::PotterRolloutLine::SessionSucceeded {
                            rounds: session_succeeded_rounds,
                            duration_secs: project_started_at.elapsed().as_secs(),
                            user_prompt_file: user_prompt_file.clone(),
                            git_commit_start: git_commit_start.clone(),
                            git_commit_end: crate::project::resolve_git_commit(&workdir),
                        },
                    ) {
                        let _ = fatal_exit_tx.send(format!(
                            "failed to write {}: {err:#}",
                            potter_rollout_path.display()
                        ));
                        break;
                    }
                    let _ = ui_event_tx.send(Event {
                        id: "".to_string(),
                        msg: EventMsg::PotterSessionSucceeded {
                            rounds: session_succeeded_rounds,
                            duration: project_started_at.elapsed(),
                            user_prompt_file: user_prompt_file.clone(),
                            git_commit_start: git_commit_start.clone(),
                            git_commit_end: crate::project::resolve_git_commit(&workdir),
                        },
                    });
                }

                if let EventMsg::PotterRoundFinished { outcome } = &event.msg
                    && let Err(err) = crate::potter_rollout::append_line(
                        &potter_rollout_path,
                        &crate::potter_rollout::PotterRolloutLine::RoundFinished {
                            outcome: outcome.clone(),
                        },
                    )
                {
                    let _ = fatal_exit_tx.send(format!(
                        "failed to write {}: {err:#}",
                        potter_rollout_path.display()
                    ));
                    break;
                }

                if ui_event_tx.send(event).is_err() {
                    break;
                }
            }
        })
    };

    let backend = tokio::spawn(crate::app_server_backend::run_app_server_backend(
        crate::app_server_backend::AppServerBackendConfig {
            codex_bin: context.codex_bin.clone(),
            developer_instructions: Some(context.developer_prompt.clone()),
            launch: context.backend_launch,
            codex_home: context.codex_compat_home.clone(),
            thread_cwd: context.thread_cwd.clone(),
            resume_thread_id,
        },
        op_rx,
        backend_event_tx,
        fatal_exit_tx,
    ));

    let exit_info = ui
        .render_turn(
            prompt,
            pad_before_first_cell,
            op_tx,
            ui_event_rx,
            fatal_exit_rx,
        )
        .await?;

    let exit_reason = exit_info.exit_reason;
    match &exit_reason {
        ExitReason::Completed => {}
        ExitReason::UserRequested | ExitReason::TaskFailed(_) | ExitReason::Fatal(_) => {
            backend.abort();
            forwarder.abort();
            let _ = backend.await;
            let _ = forwarder.await;
            return Ok(PotterRoundResult {
                exit_reason,
                stop_due_to_finite_incantatem: false,
            });
        }
    }

    backend
        .await
        .context("app-server render backend panicked")??;
    let _ = forwarder.await;

    let stop_due_to_finite_incantatem = crate::project::progress_file_has_finite_incantatem_true(
        &context.workdir,
        &context.progress_file_rel,
    )
    .context("check progress file finite_incantatem")?;

    Ok(PotterRoundResult {
        exit_reason,
        stop_due_to_finite_incantatem,
    })
}
