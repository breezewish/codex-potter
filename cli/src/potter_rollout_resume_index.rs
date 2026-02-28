use std::path::PathBuf;

use codex_protocol::ThreadId;
use codex_protocol::protocol::PotterRoundOutcome;

use crate::potter_rollout::PotterRolloutLine;

#[derive(Debug, Clone)]
pub struct PotterRolloutResumeIndex {
    pub session_started: SessionStartedIndex,
    pub completed_rounds: Vec<CompletedRoundIndex>,
    pub unfinished_round: Option<UnfinishedRoundIndex>,
}

#[derive(Debug, Clone)]
pub struct SessionStartedIndex {
    pub user_message: Option<String>,
    pub user_prompt_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CompletedRoundIndex {
    pub round_current: u32,
    pub round_total: u32,
    pub thread_id: ThreadId,
    pub rollout_path: PathBuf,
    pub session_succeeded: Option<SessionSucceededIndex>,
    pub outcome: PotterRoundOutcome,
}

#[derive(Debug, Clone)]
pub struct UnfinishedRoundIndex {
    pub round_current: u32,
    pub round_total: u32,
    pub thread_id: ThreadId,
    pub rollout_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SessionSucceededIndex {
    pub rounds: u32,
    pub duration_secs: u64,
    pub user_prompt_file: PathBuf,
    pub git_commit_start: String,
    pub git_commit_end: String,
}

pub fn build_resume_index(lines: &[PotterRolloutLine]) -> anyhow::Result<PotterRolloutResumeIndex> {
    let mut session_started: Option<SessionStartedIndex> = None;
    let mut completed_rounds: Vec<CompletedRoundIndex> = Vec::new();

    struct RoundBuilder {
        round_current: u32,
        round_total: u32,
        configured: Option<(ThreadId, PathBuf)>,
        session_succeeded: Option<SessionSucceededIndex>,
    }

    let mut current: Option<RoundBuilder> = None;

    for line in lines {
        match line {
            PotterRolloutLine::SessionStarted {
                user_message,
                user_prompt_file,
            } => {
                if session_started.is_some() || !completed_rounds.is_empty() || current.is_some() {
                    anyhow::bail!("potter-rollout: session_started must appear once at the top");
                }
                session_started = Some(SessionStartedIndex {
                    user_message: user_message.clone(),
                    user_prompt_file: user_prompt_file.clone(),
                });
            }
            PotterRolloutLine::RoundStarted {
                current: round_current,
                total: round_total,
            } => {
                if session_started.is_none() {
                    anyhow::bail!("potter-rollout: missing session_started before first round");
                }
                if current.is_some() {
                    anyhow::bail!("potter-rollout: round_started before previous round_finished");
                }
                current = Some(RoundBuilder {
                    round_current: *round_current,
                    round_total: *round_total,
                    configured: None,
                    session_succeeded: None,
                });
            }
            PotterRolloutLine::RoundConfigured {
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
            PotterRolloutLine::SessionSucceeded {
                rounds,
                duration_secs,
                user_prompt_file,
                git_commit_start,
                git_commit_end,
            } => {
                let Some(builder) = current.as_mut() else {
                    anyhow::bail!("potter-rollout: session_succeeded outside a round");
                };
                if builder.session_succeeded.is_some() {
                    anyhow::bail!("potter-rollout: duplicate session_succeeded in a single round");
                }
                builder.session_succeeded = Some(SessionSucceededIndex {
                    rounds: *rounds,
                    duration_secs: *duration_secs,
                    user_prompt_file: user_prompt_file.clone(),
                    git_commit_start: git_commit_start.clone(),
                    git_commit_end: git_commit_end.clone(),
                });
            }
            PotterRolloutLine::RoundFinished { outcome } => {
                let Some(builder) = current.take() else {
                    anyhow::bail!("potter-rollout: round_finished without round_started");
                };
                let Some((thread_id, rollout_path)) = builder.configured else {
                    anyhow::bail!("potter-rollout: round_finished without round_configured");
                };
                completed_rounds.push(CompletedRoundIndex {
                    round_current: builder.round_current,
                    round_total: builder.round_total,
                    thread_id,
                    rollout_path,
                    session_succeeded: builder.session_succeeded,
                    outcome: outcome.clone(),
                });
            }
        }
    }

    let unfinished_round = match current.take() {
        Some(builder) => {
            if builder.session_succeeded.is_some() {
                anyhow::bail!("potter-rollout: session_succeeded without round_finished at EOF");
            }
            let Some((thread_id, rollout_path)) = builder.configured else {
                anyhow::bail!("potter-rollout: missing round_configured at EOF");
            };
            Some(UnfinishedRoundIndex {
                round_current: builder.round_current,
                round_total: builder.round_total,
                thread_id,
                rollout_path,
            })
        }
        None => None,
    };

    if session_started.is_some() && completed_rounds.is_empty() && unfinished_round.is_none() {
        anyhow::bail!("potter-rollout: session_started present but no rounds found");
    }

    let Some(session_started) = session_started else {
        anyhow::bail!("potter-rollout: missing session_started before first round");
    };

    Ok(PotterRolloutResumeIndex {
        session_started,
        completed_rounds,
        unfinished_round,
    })
}
