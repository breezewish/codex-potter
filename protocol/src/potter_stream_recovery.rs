//! CodexPotter-specific helpers for recovering from transient streaming errors.
//!
//! `codex-potter` runs multi-round workflows. When Codex emits certain transient network/streaming
//! errors mid-turn (e.g. response stream disconnected), we want to keep the current round alive
//! and let the agent recover by issuing a follow-up `continue` prompt.

use crate::protocol::CodexErrorInfo;
use crate::protocol::ErrorEvent;
use crate::protocol::EventMsg;

fn parse_unexpected_status_code(message: &str) -> Option<u16> {
    let (_, rest) = message.split_once("unexpected status ")?;
    let bytes = rest.as_bytes();
    let mut end = 0usize;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == 0 {
        return None;
    }
    rest[..end].parse().ok()
}

fn is_retryable_http_status(code: u16) -> bool {
    code == 408 || code == 429 || (500..600).contains(&code)
}

/// Returns `true` when `event` represents a transient streaming/network failure.
///
/// These errors are typically recoverable by retrying the turn via a follow-up `continue`
/// prompt, instead of ending the round and starting a new one.
pub fn is_retryable_stream_error(event: &ErrorEvent) -> bool {
    match event.codex_error_info {
        Some(CodexErrorInfo::HttpConnectionFailed { .. })
        | Some(CodexErrorInfo::ResponseStreamConnectionFailed { .. })
        | Some(CodexErrorInfo::ResponseStreamDisconnected { .. })
        | Some(CodexErrorInfo::ResponseTooManyFailedAttempts { .. })
        | Some(CodexErrorInfo::InternalServerError) => true,
        _ => {
            // Best-effort fallback for older/partial servers that do not populate `codex_error_info`.
            //
            // Keep the checks tight to avoid accidentally treating unrelated errors as retryable.
            let message = event.message.as_str();
            message.contains("stream disconnected before completion")
                || message.contains("error sending request for url")
                || parse_unexpected_status_code(message).is_some_and(is_retryable_http_status)
        }
    }
}

/// Returns `true` when `msg` counts as "activity" for CodexPotter stream recovery.
///
/// Activity is defined by the workflow spec as receiving any valid:
/// - agent message
/// - tool call result
/// - reasoning output
///
/// Observing any activity resets the exponential backoff and the retry limit for future
/// streaming/network errors.
pub fn is_activity_event(msg: &EventMsg) -> bool {
    match msg {
        EventMsg::TurnComplete(ev) => ev
            .last_agent_message
            .as_ref()
            .is_some_and(|message| !message.is_empty()),
        _ => matches!(
            msg,
            EventMsg::AgentMessage(_)
                | EventMsg::AgentMessageDelta(_)
                | EventMsg::AgentReasoning(_)
                | EventMsg::AgentReasoningDelta(_)
                | EventMsg::AgentReasoningRawContent(_)
                | EventMsg::AgentReasoningRawContentDelta(_)
                | EventMsg::AgentReasoningSectionBreak(_)
                | EventMsg::ExecCommandEnd(_)
                | EventMsg::PatchApplyEnd(_)
                | EventMsg::PlanUpdate(_)
                | EventMsg::ViewImageToolCall(_)
                | EventMsg::WebSearchEnd(_)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TurnCompleteEvent;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_unexpected_status_code_extracts_code_prefix() {
        assert_eq!(
            parse_unexpected_status_code(
                "unexpected status 503 Service Unavailable: overloaded, url: https://example",
            ),
            Some(503)
        );
        assert_eq!(parse_unexpected_status_code("unexpected status foo"), None);
        assert_eq!(parse_unexpected_status_code("status 503"), None);
    }

    #[test]
    fn retryable_stream_error_accepts_unexpected_status_503() {
        let event = ErrorEvent {
            message: "unexpected status 503 Service Unavailable: overloaded".to_string(),
            codex_error_info: None,
        };
        assert!(is_retryable_stream_error(&event));
    }

    #[test]
    fn activity_event_treats_turn_complete_last_message_as_activity() {
        let msg = EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: Some("done".to_string()),
        });
        assert!(is_activity_event(&msg));

        let msg = EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
        });
        assert!(!is_activity_event(&msg));
    }
}
