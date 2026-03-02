//! Orchestrates commit-tick drains across streaming controllers.
//!
//! This module bridges queue-based chunking policy (`chunking`) with the concrete stream
//! controller (`controller`). Callers provide the current controller and tick scope; the module
//! computes queue pressure, selects a drain plan, applies it, and returns emitted history cells.

use std::time::Instant;

use crate::history_cell::HistoryCell;

use super::chunking::AdaptiveChunkingPolicy;
use super::chunking::ChunkingDecision;
use super::chunking::ChunkingMode;
use super::chunking::DrainPlan;
use super::chunking::QueueSnapshot;
use super::controller::StreamController;

/// Describes whether a commit tick may run in all modes or only in catch-up mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitTickScope {
    /// Always run the tick, regardless of current chunking mode.
    AnyMode,
    /// Run mode transitions and policy updates, but commit lines only in `CatchUp`.
    CatchUpOnly,
}

/// Describes what a single commit tick produced.
pub struct CommitTickOutput {
    /// Cells produced by drained stream lines during this tick.
    pub cells: Vec<Box<dyn HistoryCell>>,
    /// Whether at least one stream controller was present for this tick.
    pub has_controller: bool,
    /// Whether all present controllers were idle after this tick.
    pub all_idle: bool,
}

impl Default for CommitTickOutput {
    /// Creates an output that represents "no commit performed".
    ///
    /// This is used when a tick is intentionally suppressed, for example when the scope is
    /// [`CommitTickScope::CatchUpOnly`] and policy is not in catch-up mode.
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            has_controller: false,
            all_idle: true,
        }
    }
}

/// Runs one commit tick against the provided stream controller.
pub fn run_commit_tick(
    policy: &mut AdaptiveChunkingPolicy,
    stream_controller: Option<&mut StreamController>,
    scope: CommitTickScope,
    now: Instant,
) -> CommitTickOutput {
    let snapshot = stream_queue_snapshot(stream_controller.as_deref(), now);
    let decision = resolve_chunking_plan(policy, snapshot, now);
    if scope == CommitTickScope::CatchUpOnly && decision.mode != ChunkingMode::CatchUp {
        return CommitTickOutput::default();
    }

    apply_commit_tick_plan(decision.drain_plan, stream_controller)
}

fn stream_queue_snapshot(
    stream_controller: Option<&StreamController>,
    now: Instant,
) -> QueueSnapshot {
    let mut queued_lines = 0usize;
    let mut oldest_age = None;

    if let Some(controller) = stream_controller {
        queued_lines += controller.queued_lines();
        oldest_age = controller.oldest_queued_age(now);
    }

    QueueSnapshot {
        queued_lines,
        oldest_age,
    }
}

fn resolve_chunking_plan(
    policy: &mut AdaptiveChunkingPolicy,
    snapshot: QueueSnapshot,
    now: Instant,
) -> ChunkingDecision {
    let prior_mode = policy.mode();
    let decision = policy.decide(snapshot, now);
    if decision.mode != prior_mode {
        tracing::trace!(
            prior_mode = ?prior_mode,
            new_mode = ?decision.mode,
            queued_lines = snapshot.queued_lines,
            oldest_queued_age_ms = snapshot.oldest_age.map(|age| age.as_millis() as u64),
            entered_catch_up = decision.entered_catch_up,
            "stream chunking mode transition"
        );
    }
    decision
}

fn apply_commit_tick_plan(
    drain_plan: DrainPlan,
    stream_controller: Option<&mut StreamController>,
) -> CommitTickOutput {
    let mut output = CommitTickOutput::default();

    if let Some(controller) = stream_controller {
        output.has_controller = true;
        let (cell, is_idle) = drain_stream_controller(controller, drain_plan);
        if let Some(cell) = cell {
            output.cells.push(cell);
        }
        output.all_idle &= is_idle;
    }

    output
}

fn drain_stream_controller(
    controller: &mut StreamController,
    drain_plan: DrainPlan,
) -> (Option<Box<dyn HistoryCell>>, bool) {
    match drain_plan {
        DrainPlan::Single => controller.on_commit_tick(),
        DrainPlan::Batch(max_lines) => controller.on_commit_tick_batch(max_lines),
    }
}
