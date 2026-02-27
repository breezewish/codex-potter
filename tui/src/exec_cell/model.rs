use std::time::Duration;
use std::time::Instant;

use codex_protocol::parse_command::ParsedCommand;
use codex_protocol::protocol::ExecCommandSource;

#[derive(Clone, Debug, Default)]
/// Captured output from a completed exec call.
pub struct CommandOutput {
    pub exit_code: i32,
    /// The aggregated stderr + stdout interleaved.
    pub aggregated_output: String,
    /// The formatted output of the command, as seen by the model.
    pub formatted_output: String,
}

#[derive(Debug, Clone)]
/// One exec tool call, with optional output while streaming.
pub struct ExecCall {
    pub call_id: String,
    pub command: Vec<String>,
    pub parsed: Vec<ParsedCommand>,
    pub output: Option<CommandOutput>,
    pub source: ExecCommandSource,
    pub start_time: Option<Instant>,
    pub duration: Option<Duration>,
    pub interaction_input: Option<String>,
}

#[derive(Debug)]
/// A rendered "Exec" history cell, composed of one or more [`ExecCall`]s.
pub struct ExecCell {
    pub calls: Vec<ExecCall>,
    animations_enabled: bool,
}

impl ExecCell {
    pub fn new(call: ExecCall, animations_enabled: bool) -> Self {
        Self {
            calls: vec![call],
            animations_enabled,
        }
    }

    pub fn complete_call(&mut self, call_id: &str, output: CommandOutput, duration: Duration) {
        if let Some(call) = self.calls.iter_mut().rev().find(|c| c.call_id == call_id) {
            call.output = Some(output);
            call.duration = Some(duration);
            call.start_time = None;
        }
    }

    pub fn is_exploring_cell(&self) -> bool {
        self.calls.iter().all(Self::is_exploring_call)
    }

    pub fn is_active(&self) -> bool {
        self.calls.iter().any(|c| c.output.is_none())
    }

    pub fn active_start_time(&self) -> Option<Instant> {
        self.calls
            .iter()
            .find(|c| c.output.is_none())
            .and_then(|c| c.start_time)
    }

    pub fn animations_enabled(&self) -> bool {
        self.animations_enabled
    }

    pub fn iter_calls(&self) -> impl Iterator<Item = &ExecCall> {
        self.calls.iter()
    }

    pub(super) fn is_exploring_call(call: &ExecCall) -> bool {
        !matches!(call.source, ExecCommandSource::UserShell)
            && !call.parsed.is_empty()
            && call.parsed.iter().all(|p| {
                matches!(
                    p,
                    ParsedCommand::Read { .. }
                        | ParsedCommand::ListFiles { .. }
                        | ParsedCommand::Search { .. }
                )
            })
    }
}

impl ExecCall {
    pub fn is_user_shell_command(&self) -> bool {
        matches!(self.source, ExecCommandSource::UserShell)
    }

    pub fn is_unified_exec_interaction(&self) -> bool {
        matches!(self.source, ExecCommandSource::UnifiedExecInteraction)
    }
}
