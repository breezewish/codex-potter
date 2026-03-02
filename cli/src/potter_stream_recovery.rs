//! CodexPotter-specific stream recovery policy.
//!
//! CodexPotter runs multi-round workflows. When Codex emits certain transient network/streaming
//! errors mid-turn (e.g. response stream disconnected), we want to keep the current round alive
//! and let the agent recover by issuing a follow-up `continue` prompt.

use std::time::Duration;

use codex_protocol::potter_stream_recovery as protocol_recovery;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TurnCompleteEvent;

const MAX_CONTINUE_RETRIES: u32 = 10;

/// A plan to retry a failed turn by sending a follow-up `continue` prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinueRetryPlan {
    /// 1-based attempt number within the current continuous-error streak.
    pub attempt: u32,
    /// Maximum number of attempts allowed before giving up.
    pub max_attempts: u32,
    /// Backoff duration to wait before sending `continue`.
    pub backoff: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinueRetryDecision {
    Retry(ContinueRetryPlan),
    GiveUp { attempts: u32, max_attempts: u32 },
}

/// Tracks retry/backoff state for "continue after stream disconnect" behavior.
#[derive(Debug, Default)]
pub struct PotterStreamRecovery {
    continue_sends_since_activity: u32,
}

impl PotterStreamRecovery {
    pub fn new() -> Self {
        Self {
            continue_sends_since_activity: 0,
        }
    }

    /// Returns `true` when CodexPotter is still in a continuous-error retry streak.
    pub fn is_in_retry_streak(&self) -> bool {
        self.continue_sends_since_activity > 0
    }

    /// Returns `true` when `turn_complete` should be suppressed from the UI.
    ///
    /// While in a retry streak, Codex can emit an empty `TurnComplete` that corresponds to a
    /// transient stream/network failure. CodexPotter keeps the round alive by retrying with
    /// follow-up `continue` turns, so the UI must not interpret the empty `TurnComplete` as the
    /// end of the round.
    pub fn should_suppress_turn_complete(&self, turn_complete: &TurnCompleteEvent) -> bool {
        self.is_in_retry_streak()
            && turn_complete
                .last_agent_message
                .as_deref()
                .is_none_or(|message| message.is_empty())
    }

    /// Observe a non-error event and reset backoff state when we see activity.
    pub fn observe_event(&mut self, msg: &EventMsg) {
        if protocol_recovery::is_activity_event(msg) {
            self.continue_sends_since_activity = 0;
        }
    }

    /// If `error` is retryable, returns a decision describing whether to retry (and how) or give
    /// up due to exceeding the retry cap.
    pub fn plan_retry(&mut self, error: &ErrorEvent) -> Option<ContinueRetryDecision> {
        if !protocol_recovery::is_retryable_stream_error(error) {
            return None;
        }

        if self.continue_sends_since_activity >= MAX_CONTINUE_RETRIES {
            return Some(ContinueRetryDecision::GiveUp {
                attempts: self.continue_sends_since_activity,
                max_attempts: MAX_CONTINUE_RETRIES,
            });
        }

        let attempt = self.continue_sends_since_activity + 1;
        let backoff = Duration::from_secs(if self.continue_sends_since_activity == 0 {
            0
        } else {
            1u64 << (self.continue_sends_since_activity - 1)
        });
        self.continue_sends_since_activity += 1;

        Some(ContinueRetryDecision::Retry(ContinueRetryPlan {
            attempt,
            max_attempts: MAX_CONTINUE_RETRIES,
            backoff,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::AgentMessageDeltaEvent;
    use codex_protocol::protocol::CodexErrorInfo;
    use pretty_assertions::assert_eq;

    fn retryable_error_event() -> ErrorEvent {
        ErrorEvent {
            message: "stream disconnected before completion: error sending request for url (...)"
                .to_string(),
            codex_error_info: Some(CodexErrorInfo::ResponseStreamDisconnected {
                http_status_code: None,
            }),
        }
    }

    #[test]
    fn plan_retry_sends_immediately_then_backs_off_exponentially() {
        let mut state = PotterStreamRecovery::new();
        let err = retryable_error_event();

        let mut plans = Vec::new();
        for _ in 0..3 {
            let Some(ContinueRetryDecision::Retry(plan)) = state.plan_retry(&err) else {
                panic!("expected retry plan");
            };
            plans.push(plan);
        }

        assert_eq!(
            plans,
            vec![
                ContinueRetryPlan {
                    attempt: 1,
                    max_attempts: 10,
                    backoff: Duration::from_secs(0),
                },
                ContinueRetryPlan {
                    attempt: 2,
                    max_attempts: 10,
                    backoff: Duration::from_secs(1),
                },
                ContinueRetryPlan {
                    attempt: 3,
                    max_attempts: 10,
                    backoff: Duration::from_secs(2),
                },
            ]
        );
    }

    #[test]
    fn observe_event_resets_retry_budget_on_activity() {
        let mut state = PotterStreamRecovery::new();
        let err = retryable_error_event();

        let Some(ContinueRetryDecision::Retry(first)) = state.plan_retry(&err) else {
            panic!("expected retry plan");
        };
        assert_eq!(first.attempt, 1);

        let Some(ContinueRetryDecision::Retry(second)) = state.plan_retry(&err) else {
            panic!("expected retry plan");
        };
        assert_eq!(second.attempt, 2);

        state.observe_event(&EventMsg::AgentMessageDelta(AgentMessageDeltaEvent {
            delta: "hello".to_string(),
        }));

        let Some(ContinueRetryDecision::Retry(reset)) = state.plan_retry(&err) else {
            panic!("expected retry plan");
        };
        assert_eq!(reset.attempt, 1);
    }

    #[test]
    fn plan_retry_gives_up_after_ten_attempts() {
        let mut state = PotterStreamRecovery::new();
        let err = retryable_error_event();

        for _ in 0..10 {
            let Some(ContinueRetryDecision::Retry(_)) = state.plan_retry(&err) else {
                panic!("expected retry plan");
            };
        }

        let Some(ContinueRetryDecision::GiveUp {
            attempts,
            max_attempts,
        }) = state.plan_retry(&err)
        else {
            panic!("expected give up decision");
        };
        assert_eq!((attempts, max_attempts), (10, 10));
    }

    #[test]
    fn should_suppress_turn_complete_during_retry_streak() {
        let mut state = PotterStreamRecovery::new();
        let err = retryable_error_event();

        let Some(ContinueRetryDecision::Retry(_)) = state.plan_retry(&err) else {
            panic!("expected retry plan");
        };

        assert!(state.should_suppress_turn_complete(&TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
        }));

        assert!(!state.should_suppress_turn_complete(&TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: Some("done".to_string()),
        }));
    }
}
