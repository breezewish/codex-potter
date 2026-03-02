use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use crate::app_server_protocol::ApplyPatchApprovalResponse;
use crate::app_server_protocol::ClientInfo;
use crate::app_server_protocol::ClientNotification;
use crate::app_server_protocol::ClientRequest;
use crate::app_server_protocol::CommandExecutionApprovalDecision;
use crate::app_server_protocol::CommandExecutionRequestApprovalResponse;
use crate::app_server_protocol::ExecCommandApprovalResponse;
use crate::app_server_protocol::FileChangeApprovalDecision;
use crate::app_server_protocol::FileChangeRequestApprovalResponse;
use crate::app_server_protocol::InitializeParams;
use crate::app_server_protocol::JSONRPCError;
use crate::app_server_protocol::JSONRPCErrorError;
use crate::app_server_protocol::JSONRPCMessage;
use crate::app_server_protocol::JSONRPCResponse;
use crate::app_server_protocol::RequestId;
use crate::app_server_protocol::ServerRequest;
use crate::app_server_protocol::ThreadResumeParams;
use crate::app_server_protocol::ThreadResumeResponse;
use crate::app_server_protocol::ThreadRollbackParams;
use crate::app_server_protocol::ThreadRollbackResponse;
use crate::app_server_protocol::ThreadStartParams;
use crate::app_server_protocol::ThreadStartResponse;
use crate::app_server_protocol::TurnStartParams;
use crate::app_server_protocol::TurnStartResponse;
use crate::app_server_protocol::UserInput as ApiUserInput;
use crate::potter_stream_recovery::ContinueRetryDecision;
use crate::potter_stream_recovery::ContinueRetryPlan;
use crate::potter_stream_recovery::PotterStreamRecovery;
use anyhow::Context;
use codex_protocol::ThreadId;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::PotterRoundOutcome;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::protocol::SessionConfiguredEvent;
use codex_protocol::user_input::UserInput as CodexUserInput;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::ChildStderr;
use tokio::process::ChildStdin;
use tokio::process::ChildStdout;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryAction {
    RetryContinue { attempt: u32 },
}

struct StreamRecoveryContext {
    stream_recovery: PotterStreamRecovery,
    recovery_action_tx: UnboundedSender<RecoveryAction>,
    pending_continue_retry: Option<ContinueRetryPlan>,
    has_sent_turn_start: bool,
    has_finished_round: bool,
    last_turn_start_was_recovery_continue: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppServerLaunchConfig {
    pub spawn_sandbox: Option<crate::app_server_protocol::SandboxMode>,
    pub thread_sandbox: Option<crate::app_server_protocol::SandboxMode>,
    pub bypass_approvals_and_sandbox: bool,
}

impl AppServerLaunchConfig {
    pub fn from_cli(sandbox: crate::CliSandbox, bypass: bool) -> Self {
        if bypass {
            return Self {
                spawn_sandbox: None,
                thread_sandbox: Some(crate::app_server_protocol::SandboxMode::DangerFullAccess),
                bypass_approvals_and_sandbox: true,
            };
        }

        let mode = sandbox.as_protocol();
        Self {
            spawn_sandbox: mode,
            thread_sandbox: mode,
            bypass_approvals_and_sandbox: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerBackendConfig {
    pub codex_bin: String,
    pub developer_instructions: Option<String>,
    pub launch: AppServerLaunchConfig,
    pub codex_home: Option<PathBuf>,
    pub thread_cwd: Option<PathBuf>,
    pub resume_thread_id: Option<ThreadId>,
}

pub async fn run_app_server_backend(
    config: AppServerBackendConfig,
    mut op_rx: UnboundedReceiver<Op>,
    event_tx: UnboundedSender<Event>,
    fatal_exit_tx: UnboundedSender<String>,
) -> anyhow::Result<()> {
    match run_app_server_backend_inner(config, &mut op_rx, &event_tx, &fatal_exit_tx).await {
        Ok(()) => Ok(()),
        Err(err) => {
            let message = format!("Failed to run `codex app-server`: {err}");
            let _ = event_tx.send(Event {
                id: "".to_string(),
                msg: EventMsg::Error(ErrorEvent {
                    message: message.clone(),
                    codex_error_info: None,
                }),
            });
            let _ = fatal_exit_tx.send(message);

            // Surface backend failures via the UI and exit reason, instead of bubbling up an
            // additional anyhow error that would get printed after the TUI exits.
            Ok(())
        }
    }
}

async fn run_app_server_backend_inner(
    config: AppServerBackendConfig,
    op_rx: &mut UnboundedReceiver<Op>,
    event_tx: &UnboundedSender<Event>,
    fatal_exit_tx: &UnboundedSender<String>,
) -> anyhow::Result<()> {
    let AppServerBackendConfig {
        codex_bin,
        developer_instructions,
        launch,
        codex_home,
        thread_cwd,
        resume_thread_id,
    } = config;
    let (mut child, stdin, stdout, stderr) =
        spawn_app_server(&codex_bin, launch, codex_home.as_deref()).await?;
    let stderr_capture = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_truncated = Arc::new(AtomicBool::new(false));
    let stderr_task = {
        let stderr_capture = stderr_capture.clone();
        let stderr_truncated = stderr_truncated.clone();
        tokio::spawn(async move {
            const LIMIT_BYTES: usize = 32 * 1024;
            let mut stderr = stderr;
            let mut buf = [0u8; 4096];

            loop {
                let n = stderr.read(&mut buf).await?;
                if n == 0 {
                    break;
                }

                let mut capture = match stderr_capture.lock() {
                    Ok(guard) => guard,
                    Err(err) => err.into_inner(),
                };
                let remaining = LIMIT_BYTES.saturating_sub(capture.len());
                if remaining == 0 {
                    stderr_truncated.store(true, Ordering::Relaxed);
                    continue;
                }

                let take = remaining.min(n);
                capture.extend_from_slice(&buf[..take]);
                if take < n {
                    stderr_truncated.store(true, Ordering::Relaxed);
                }
            }

            Ok::<(), std::io::Error>(())
        })
    };

    let mut stdin = Some(stdin);
    let mut lines = BufReader::new(stdout).lines();
    let mut next_id: i64 = 1;
    let mut shutdown_requested = false;
    let (recovery_action_tx, mut recovery_action_rx) = unbounded_channel::<RecoveryAction>();
    let mut recovery = StreamRecoveryContext {
        stream_recovery: PotterStreamRecovery::new(),
        recovery_action_tx,
        pending_continue_retry: None,
        has_sent_turn_start: false,
        has_finished_round: false,
        last_turn_start_was_recovery_continue: false,
    };

    let result = async {
        initialize_app_server(
            stdin
                .as_mut()
                .context("codex app-server stdin unavailable")?,
            &mut lines,
            &mut next_id,
            &mut recovery,
            event_tx,
        )
        .await?;

        let thread_start_or_resume = match resume_thread_id {
            Some(thread_id) => ThreadStartOrResume::Resume(
                thread_resume(
                    stdin
                        .as_mut()
                        .context("codex app-server stdin unavailable")?,
                    &mut lines,
                    &mut next_id,
                    ThreadResumeSettings {
                        thread_id,
                        developer_instructions,
                        sandbox_mode: launch.thread_sandbox,
                        cwd: thread_cwd,
                    },
                    &mut recovery,
                    event_tx,
                )
                .await?,
            ),
            None => ThreadStartOrResume::Start(
                thread_start(
                    stdin
                        .as_mut()
                        .context("codex app-server stdin unavailable")?,
                    &mut lines,
                    &mut next_id,
                    ThreadStartSettings {
                        developer_instructions,
                        sandbox_mode: launch.thread_sandbox,
                        cwd: thread_cwd,
                    },
                    &mut recovery,
                    event_tx,
                )
                .await?,
            ),
        };

        let thread_id = thread_start_or_resume.thread_id().to_string();

        let session_configured = synthesize_session_configured(&thread_start_or_resume)?;
        let _ = event_tx.send(Event {
            id: "".to_string(),
            msg: EventMsg::SessionConfigured(session_configured),
        });

        loop {
            tokio::select! {
                maybe_op = op_rx.recv(), if !shutdown_requested => {
                    let Some(op) = maybe_op else {
                        shutdown_requested = true;
                        stdin.take();
                        continue;
                    };
                    if matches!(op, Op::UserInput { .. }) {
                        let was_in_retry_streak = recovery.stream_recovery.is_in_retry_streak();
                        recovery.has_sent_turn_start = true;
                        recovery.last_turn_start_was_recovery_continue = false;
                        recovery.pending_continue_retry = None;
                        recovery.stream_recovery = PotterStreamRecovery::new();
                        if was_in_retry_streak {
                            let _ = event_tx.send(Event {
                                id: "".to_string(),
                                msg: EventMsg::PotterStreamRecoveryRecovered,
                            });
                        }
                    }
                    handle_op(
                        &thread_id,
                        op,
                        stdin.as_mut().context("codex app-server stdin unavailable")?,
                        &mut lines,
                        &mut next_id,
                        &mut recovery,
                        event_tx,
                    )
                    .await?;
                }
                maybe_action = recovery_action_rx.recv(), if !shutdown_requested => {
                    let Some(action) = maybe_action else {
                        continue;
                    };

                    if !recovery.stream_recovery.is_in_retry_streak() {
                        continue;
                    }

                    match action {
                        RecoveryAction::RetryContinue { attempt } => {
                            recovery.has_sent_turn_start = true;
                            if attempt >= 2 && recovery.last_turn_start_was_recovery_continue {
                                // Remove the previous automatic `Continue` turn from the thread
                                // history so retries do not accumulate redundant `Continue`
                                // prompts in the model context.
                                //
                                // This is expected to succeed because stream recovery retries are
                                // scheduled only after the previous turn has ended.
                                thread_rollback(
                                    &thread_id,
                                    stdin.as_mut().context("codex app-server stdin unavailable")?,
                                    &mut lines,
                                    &mut next_id,
                                    &mut recovery,
                                    event_tx,
                                )
                                .await?;
                            }
                            recovery.last_turn_start_was_recovery_continue = true;
                            handle_op(
                                &thread_id,
                                Op::UserInput {
                                    items: vec![CodexUserInput::Text {
                                        text: String::from("Continue"),
                                        text_elements: Vec::new(),
                                    }],
                                    final_output_json_schema: None,
                                },
                                stdin.as_mut().context("codex app-server stdin unavailable")?,
                                &mut lines,
                                &mut next_id,
                                &mut recovery,
                                event_tx,
                            )
                            .await?;
                        }
                    }
                }
                maybe_line = lines.next_line() => {
                    let Some(line) = maybe_line? else {
                        break;
                    };
                    let msg: JSONRPCMessage = serde_json::from_str(&line)
                        .with_context(|| format!("failed to decode app-server message: {line}"))?;
                    handle_app_server_message(
                        msg,
                        &mut stdin,
                        &mut recovery,
                        event_tx,
                    )
                    .await?;
                }
            }
        }

        let _ = child.wait().await;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if result.is_err() {
        // Do not await the drain task on failure: the child might keep running and we'd hang while
        // waiting for stderr to close. We already captured enough to provide context.
        stderr_task.abort();
    } else {
        let _ = stderr_task.await;
    }

    result.map_err(|err| {
        let stderr = {
            let capture = match stderr_capture.lock() {
                Ok(guard) => guard,
                Err(err) => err.into_inner(),
            };
            String::from_utf8_lossy(&capture).to_string()
        };

        let stderr = stderr.trim_end_matches(['\n', '\r']).to_string();
        if stderr.is_empty() {
            return err;
        }

        let mut message = String::new();
        message.push_str(&err.to_string());
        message.push_str("\n\n");
        message.push_str("app-server stderr:");
        message.push('\n');
        message.push_str(&stderr);
        if stderr_truncated.load(Ordering::Relaxed) {
            message.push('\n');
            message.push_str("[stderr truncated]");
        }
        anyhow::Error::msg(message)
    })?;

    // If the backend finishes while the UI still expects it to be alive, ensure the UI can exit.
    if !shutdown_requested {
        let message = "codex app-server exited unexpectedly".to_string();
        let _ = event_tx.send(Event {
            id: "".to_string(),
            msg: EventMsg::Error(ErrorEvent {
                message: message.clone(),
                codex_error_info: None,
            }),
        });
        let _ = fatal_exit_tx.send(message);
    }

    Ok(())
}

async fn spawn_app_server(
    codex_bin: &str,
    launch: AppServerLaunchConfig,
    codex_home: Option<&Path>,
) -> anyhow::Result<(Child, ChildStdin, ChildStdout, ChildStderr)> {
    let mut cmd = Command::new(codex_bin);
    cmd.kill_on_drop(true);

    if let Some(codex_home) = codex_home {
        cmd.env("CODEX_HOME", codex_home);
    }

    if launch.bypass_approvals_and_sandbox {
        cmd.arg("--dangerously-bypass-approvals-and-sandbox");
    }
    if let Some(mode) = launch.spawn_sandbox {
        cmd.arg("--sandbox");
        cmd.arg(match mode {
            crate::app_server_protocol::SandboxMode::ReadOnly => "read-only",
            crate::app_server_protocol::SandboxMode::WorkspaceWrite => "workspace-write",
            crate::app_server_protocol::SandboxMode::DangerFullAccess => "danger-full-access",
        });
    }

    let mut child = cmd
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start `{codex_bin}` app-server"))?;

    let stdin = child
        .stdin
        .take()
        .context("codex app-server stdin unavailable")?;
    let stdout = child
        .stdout
        .take()
        .context("codex app-server stdout unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("codex app-server stderr unavailable")?;
    Ok((child, stdin, stdout, stderr))
}

async fn initialize_app_server(
    stdin: &mut ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    next_id: &mut i64,
    recovery: &mut StreamRecoveryContext,
    event_tx: &UnboundedSender<Event>,
) -> anyhow::Result<()> {
    let request_id = next_request_id(next_id);
    let request = ClientRequest::Initialize {
        request_id: request_id.clone(),
        params: InitializeParams {
            client_info: ClientInfo {
                name: "codex-potter".to_string(),
                title: Some("codex-potter".to_string()),
                version: codex_tui::CODEX_POTTER_VERSION.to_string(),
            },
        },
    };
    send_message(stdin, &request).await?;
    let _response = read_until_response(stdin, lines, request_id, recovery, event_tx).await?;

    send_message(stdin, &ClientNotification::Initialized).await?;
    Ok(())
}

struct ThreadStartSettings {
    developer_instructions: Option<String>,
    sandbox_mode: Option<crate::app_server_protocol::SandboxMode>,
    cwd: Option<PathBuf>,
}

struct ThreadResumeSettings {
    thread_id: ThreadId,
    developer_instructions: Option<String>,
    sandbox_mode: Option<crate::app_server_protocol::SandboxMode>,
    cwd: Option<PathBuf>,
}

async fn thread_start(
    stdin: &mut ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    next_id: &mut i64,
    settings: ThreadStartSettings,
    recovery: &mut StreamRecoveryContext,
    event_tx: &UnboundedSender<Event>,
) -> anyhow::Result<ThreadStartResponse> {
    let ThreadStartSettings {
        developer_instructions,
        sandbox_mode,
        cwd,
    } = settings;
    let request_id = next_request_id(next_id);
    let request = ClientRequest::ThreadStart {
        request_id: request_id.clone(),
        params: ThreadStartParams {
            model: None,
            model_provider: None,
            cwd: cwd.map(|cwd| cwd.to_string_lossy().to_string()),
            approval_policy: Some(crate::app_server_protocol::AskForApproval::Never),
            sandbox: sandbox_mode,
            config: None,
            base_instructions: None,
            developer_instructions,
            experimental_raw_events: false,
        },
    };
    send_message(stdin, &request).await?;
    let response = read_until_response(stdin, lines, request_id, recovery, event_tx).await?;
    serde_json::from_value(response.result).context("decode thread/start response")
}

async fn thread_resume(
    stdin: &mut ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    next_id: &mut i64,
    settings: ThreadResumeSettings,
    recovery: &mut StreamRecoveryContext,
    event_tx: &UnboundedSender<Event>,
) -> anyhow::Result<ThreadResumeResponse> {
    let ThreadResumeSettings {
        thread_id,
        developer_instructions,
        sandbox_mode,
        cwd,
    } = settings;

    let request_id = next_request_id(next_id);
    let request = ClientRequest::ThreadResume {
        request_id: request_id.clone(),
        params: ThreadResumeParams {
            thread_id: thread_id.to_string(),
            model: None,
            model_provider: None,
            cwd: cwd.map(|cwd| cwd.to_string_lossy().to_string()),
            approval_policy: Some(crate::app_server_protocol::AskForApproval::Never),
            sandbox: sandbox_mode,
            config: None,
            base_instructions: None,
            developer_instructions,
        },
    };
    send_message(stdin, &request).await?;
    let response = read_until_response(stdin, lines, request_id, recovery, event_tx).await?;
    serde_json::from_value(response.result).context("decode thread/resume response")
}

async fn thread_rollback(
    thread_id: &str,
    stdin: &mut ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    next_id: &mut i64,
    recovery: &mut StreamRecoveryContext,
    event_tx: &UnboundedSender<Event>,
) -> anyhow::Result<()> {
    let request_id = next_request_id(next_id);
    let request = ClientRequest::ThreadRollback {
        request_id: request_id.clone(),
        params: ThreadRollbackParams {
            thread_id: thread_id.to_string(),
            num_turns: 1,
        },
    };
    send_message(stdin, &request).await?;
    let response = read_until_response(stdin, lines, request_id, recovery, event_tx)
        .await
        .with_context(|| format!("thread/rollback thread_id={thread_id}"))?;
    let _parsed: ThreadRollbackResponse =
        serde_json::from_value(response.result).context("decode thread/rollback response")?;
    Ok(())
}

async fn handle_op(
    thread_id: &str,
    op: Op,
    stdin: &mut ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    next_id: &mut i64,
    recovery: &mut StreamRecoveryContext,
    event_tx: &UnboundedSender<Event>,
) -> anyhow::Result<()> {
    match op {
        Op::UserInput {
            items,
            final_output_json_schema,
        } => {
            let request_id = next_request_id(next_id);
            let input = items.into_iter().map(ApiUserInput::from).collect();
            let request = ClientRequest::TurnStart {
                request_id: request_id.clone(),
                params: TurnStartParams {
                    thread_id: thread_id.to_string(),
                    input,
                    cwd: None,
                    approval_policy: None,
                    sandbox_policy: None,
                    model: None,
                    effort: None,
                    summary: None,
                    output_schema: final_output_json_schema,
                    collaboration_mode: None,
                },
            };
            send_message(stdin, &request).await?;
            let response =
                read_until_response(stdin, lines, request_id, recovery, event_tx).await?;
            let _parsed: TurnStartResponse =
                serde_json::from_value(response.result).context("decode turn/start response")?;
            Ok(())
        }
        Op::Interrupt => {
            // The single-turn TUI runner does not track the active turn id, so we cannot call
            // turn/interrupt. Ignore and let the session complete naturally.
            Ok(())
        }
        Op::GetHistoryEntryRequest { .. } => {
            // The prompt screen does not support fetching persisted prompt history from the
            // backend. Ignore the request so the UI can stay simple.
            Ok(())
        }
    }
}

async fn handle_app_server_message(
    msg: JSONRPCMessage,
    stdin: &mut Option<ChildStdin>,
    recovery: &mut StreamRecoveryContext,
    event_tx: &UnboundedSender<Event>,
) -> anyhow::Result<()> {
    match msg {
        JSONRPCMessage::Notification(notification) => {
            handle_codex_event_notification(
                &notification.method,
                notification.params,
                recovery,
                event_tx,
            )?;
        }
        JSONRPCMessage::Request(request) => {
            if let Some(stdin) = stdin.as_mut() {
                handle_server_request(stdin, request).await?;
            }
        }
        JSONRPCMessage::Response(_) | JSONRPCMessage::Error(_) => {}
    }

    Ok(())
}

fn handle_codex_event_notification(
    method: &str,
    params: Option<serde_json::Value>,
    recovery: &mut StreamRecoveryContext,
    event_tx: &UnboundedSender<Event>,
) -> anyhow::Result<()> {
    if !method.starts_with("codex/event/") {
        return Ok(());
    }
    let Some(params) = params else {
        return Ok(());
    };

    let event: Event = serde_json::from_value(params)?;
    handle_codex_event(event, recovery, event_tx);
    Ok(())
}

fn handle_codex_event(
    event: Event,
    recovery: &mut StreamRecoveryContext,
    event_tx: &UnboundedSender<Event>,
) {
    // `codex-potter` uses `thread/rollback` internally to ensure stream recovery retries do not
    // accumulate redundant automatic `Continue` turns in the thread history. The app-server
    // forwards raw `codex/event/*` notifications for every core event, including rollback
    // lifecycle events that are UI-irrelevant (or even confusing) for CodexPotter users.
    if matches!(event.msg, EventMsg::ThreadRolledBack(_)) {
        return;
    }

    let event_id = event.id.clone();
    let mut should_forward = true;
    let mut round_outcome: Option<PotterRoundOutcome> = None;

    let was_in_retry_streak = recovery.stream_recovery.is_in_retry_streak();
    let should_suppress_turn_complete = match &event.msg {
        EventMsg::TurnComplete(ev) => recovery.stream_recovery.should_suppress_turn_complete(ev),
        _ => false,
    };

    recovery.stream_recovery.observe_event(&event.msg);

    if was_in_retry_streak && !recovery.stream_recovery.is_in_retry_streak() {
        recovery.pending_continue_retry = None;
        let _ = event_tx.send(Event {
            id: event.id.clone(),
            msg: EventMsg::PotterStreamRecoveryRecovered,
        });
    }

    if let EventMsg::Error(err) = &event.msg
        && recovery.has_sent_turn_start
    {
        if recovery.pending_continue_retry.is_some()
            && codex_protocol::potter_stream_recovery::is_retryable_stream_error(err)
        {
            // A retryable error was already observed for the current turn. Wait for TurnComplete
            // and then issue the planned automatic `Continue`.
            should_forward = false;
        } else if recovery.pending_continue_retry.is_none()
            && let Some(decision) = recovery.stream_recovery.plan_retry(err)
        {
            match decision {
                ContinueRetryDecision::Retry(plan) => {
                    let _ = event_tx.send(Event {
                        id: event.id.clone(),
                        msg: EventMsg::PotterStreamRecoveryUpdate {
                            attempt: plan.attempt,
                            max_attempts: plan.max_attempts,
                            error_message: err.message.clone(),
                        },
                    });
                    recovery.pending_continue_retry = Some(plan);
                }
                ContinueRetryDecision::GiveUp {
                    attempts,
                    max_attempts,
                } => {
                    let _ = event_tx.send(Event {
                        id: event.id.clone(),
                        msg: EventMsg::PotterStreamRecoveryGaveUp {
                            error_message: err.message.clone(),
                            attempts,
                            max_attempts,
                        },
                    });
                    round_outcome = Some(PotterRoundOutcome::TaskFailed {
                        message: format!(
                            "{} (stream recovery gave up after {attempts}/{max_attempts} retries)",
                            err.message
                        ),
                    });
                }
            }

            should_forward = false;
        }
    }

    if matches!(event.msg, EventMsg::TurnAborted(_)) {
        recovery.pending_continue_retry = None;
    }

    if matches!(event.msg, EventMsg::TurnComplete(_))
        && let Some(plan) = recovery.pending_continue_retry.take()
    {
        let tx = recovery.recovery_action_tx.clone();
        let action = RecoveryAction::RetryContinue {
            attempt: plan.attempt,
        };
        if plan.backoff.is_zero() {
            let _ = tx.send(action);
        } else {
            tokio::spawn(async move {
                tokio::time::sleep(plan.backoff).await;
                let _ = tx.send(action);
            });
        }
    }

    if should_suppress_turn_complete {
        should_forward = false;
    }

    if round_outcome.is_none() {
        round_outcome = match &event.msg {
            EventMsg::TurnComplete(_) if !should_suppress_turn_complete => {
                Some(PotterRoundOutcome::Completed)
            }
            EventMsg::TurnAborted(ev)
                if !matches!(
                    ev.reason,
                    codex_protocol::protocol::TurnAbortReason::Replaced
                ) =>
            {
                Some(PotterRoundOutcome::UserRequested)
            }
            EventMsg::Error(err) if should_forward => Some(PotterRoundOutcome::Fatal {
                message: err.message.clone(),
            }),
            _ => None,
        };
    }

    if should_forward {
        let _ = event_tx.send(event);
    }

    if !recovery.has_finished_round
        && let Some(outcome) = round_outcome
    {
        recovery.has_finished_round = true;
        let _ = event_tx.send(Event {
            id: event_id,
            msg: EventMsg::PotterRoundFinished { outcome },
        });
    }
}

async fn handle_server_request(
    stdin: &mut ChildStdin,
    request: crate::app_server_protocol::JSONRPCRequest,
) -> anyhow::Result<()> {
    let request_id = request.id.clone();
    let method = request.method.clone();
    let server_request = match ServerRequest::try_from(request) {
        Ok(request) => request,
        Err(err) => {
            let message = format!("unsupported server request {method:?}: {err}");
            send_message(
                stdin,
                &JSONRPCMessage::Error(JSONRPCError {
                    error: JSONRPCErrorError {
                        code: -32601,
                        message,
                        data: None,
                    },
                    id: request_id,
                }),
            )
            .await?;
            return Ok(());
        }
    };

    match server_request {
        ServerRequest::CommandExecution { .. } => {
            let response = CommandExecutionRequestApprovalResponse {
                decision: CommandExecutionApprovalDecision::Accept,
            };
            send_response(stdin, request_id, response).await?;
        }
        ServerRequest::FileChange { .. } => {
            let response = FileChangeRequestApprovalResponse {
                decision: FileChangeApprovalDecision::Accept,
            };
            send_response(stdin, request_id, response).await?;
        }
        ServerRequest::ApplyPatch { .. } => {
            let response = ApplyPatchApprovalResponse {
                decision: ReviewDecision::Approved,
            };
            send_response(stdin, request_id, response).await?;
        }
        ServerRequest::ExecCommand { .. } => {
            let response = ExecCommandApprovalResponse {
                decision: ReviewDecision::Approved,
            };
            send_response(stdin, request_id, response).await?;
        }
    }

    Ok(())
}

async fn send_message<T>(stdin: &mut ChildStdin, message: &T) -> anyhow::Result<()>
where
    T: serde::Serialize,
{
    let json = serde_json::to_vec(message)?;
    stdin.write_all(&json).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    Ok(())
}

async fn send_response<T>(
    stdin: &mut ChildStdin,
    request_id: RequestId,
    response: T,
) -> anyhow::Result<()>
where
    T: serde::Serialize,
{
    send_message(
        stdin,
        &JSONRPCMessage::Response(JSONRPCResponse {
            id: request_id,
            result: serde_json::to_value(response)?,
        }),
    )
    .await
}

async fn read_until_response(
    stdin: &mut ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    request_id: RequestId,
    recovery: &mut StreamRecoveryContext,
    event_tx: &UnboundedSender<Event>,
) -> anyhow::Result<JSONRPCResponse> {
    match read_until_response_or_error(stdin, lines, &request_id, recovery, event_tx).await? {
        Ok(response) => Ok(response),
        Err(error) => {
            anyhow::bail!("app-server returned error for {request_id:?}: {error:?}");
        }
    }
}

async fn read_until_response_or_error(
    stdin: &mut ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    request_id: &RequestId,
    recovery: &mut StreamRecoveryContext,
    event_tx: &UnboundedSender<Event>,
) -> anyhow::Result<Result<JSONRPCResponse, JSONRPCErrorError>> {
    loop {
        let Some(line) = lines.next_line().await? else {
            anyhow::bail!("app-server stdout closed while waiting for response {request_id:?}");
        };
        let msg: JSONRPCMessage =
            serde_json::from_str(&line).with_context(|| format!("decode json-rpc: {line}"))?;

        match msg {
            JSONRPCMessage::Response(response) if &response.id == request_id => {
                return Ok(Ok(response));
            }
            JSONRPCMessage::Error(err) if &err.id == request_id => return Ok(Err(err.error)),
            JSONRPCMessage::Notification(notification) => {
                handle_codex_event_notification(
                    &notification.method,
                    notification.params,
                    recovery,
                    event_tx,
                )?;
            }
            JSONRPCMessage::Request(request) => {
                handle_server_request(stdin, request).await?;
            }
            _ => {}
        }
    }
}

fn synthesize_session_configured(
    thread_start_or_resume: &ThreadStartOrResume,
) -> anyhow::Result<SessionConfiguredEvent> {
    let thread_id =
        ThreadId::from_string(thread_start_or_resume.thread_id()).context("parse thread id")?;

    Ok(SessionConfiguredEvent {
        session_id: thread_id,
        forked_from_id: None,
        model: thread_start_or_resume.model().to_string(),
        model_provider_id: thread_start_or_resume.model_provider().to_string(),
        cwd: thread_start_or_resume.cwd().to_path_buf(),
        reasoning_effort: thread_start_or_resume.reasoning_effort(),
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        rollout_path: thread_start_or_resume.rollout_path().to_path_buf(),
    })
}

enum ThreadStartOrResume {
    Start(ThreadStartResponse),
    Resume(ThreadResumeResponse),
}

impl ThreadStartOrResume {
    fn thread_id(&self) -> &str {
        match self {
            ThreadStartOrResume::Start(resp) => &resp.thread.id,
            ThreadStartOrResume::Resume(resp) => &resp.thread.id,
        }
    }

    fn model(&self) -> &str {
        match self {
            ThreadStartOrResume::Start(resp) => &resp.model,
            ThreadStartOrResume::Resume(resp) => &resp.model,
        }
    }

    fn model_provider(&self) -> &str {
        match self {
            ThreadStartOrResume::Start(resp) => &resp.model_provider,
            ThreadStartOrResume::Resume(resp) => &resp.model_provider,
        }
    }

    fn cwd(&self) -> &Path {
        match self {
            ThreadStartOrResume::Start(resp) => resp.cwd.as_path(),
            ThreadStartOrResume::Resume(resp) => resp.cwd.as_path(),
        }
    }

    fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        match self {
            ThreadStartOrResume::Start(resp) => resp.reasoning_effort,
            ThreadStartOrResume::Resume(resp) => resp.reasoning_effort,
        }
    }

    fn rollout_path(&self) -> &Path {
        match self {
            ThreadStartOrResume::Start(resp) => resp.thread.path.as_path(),
            ThreadStartOrResume::Resume(resp) => resp.thread.path.as_path(),
        }
    }
}

fn next_request_id(next_id: &mut i64) -> RequestId {
    let id = *next_id;
    *next_id += 1;
    RequestId::Integer(id)
}

#[cfg(test)]
mod stream_recovery_tests {
    use super::*;
    use codex_protocol::protocol::AgentMessageDeltaEvent;
    use codex_protocol::protocol::CodexErrorInfo;
    use codex_protocol::protocol::ThreadRolledBackEvent;
    use codex_protocol::protocol::TurnCompleteEvent;
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
    fn stream_recovery_translates_retryable_error_to_potter_events() {
        let (event_tx, mut event_rx) = unbounded_channel::<Event>();
        let (action_tx, mut action_rx) = unbounded_channel::<RecoveryAction>();
        let mut recovery = StreamRecoveryContext {
            stream_recovery: PotterStreamRecovery::new(),
            recovery_action_tx: action_tx,
            pending_continue_retry: None,
            has_sent_turn_start: true,
            has_finished_round: false,
            last_turn_start_was_recovery_continue: false,
        };

        handle_codex_event(
            Event {
                id: "err".into(),
                msg: EventMsg::Error(retryable_error_event()),
            },
            &mut recovery,
            &event_tx,
        );

        let event = event_rx.try_recv().expect("expected injected event");
        let EventMsg::PotterStreamRecoveryUpdate {
            attempt,
            max_attempts,
            error_message,
        } = event.msg
        else {
            panic!("expected PotterStreamRecoveryUpdate, got: {:?}", event.msg);
        };
        assert_eq!(attempt, 1);
        assert_eq!(max_attempts, 10);
        assert_eq!(
            error_message,
            "stream disconnected before completion: error sending request for url (...)"
        );

        assert!(
            event_rx.try_recv().is_err(),
            "expected retryable Error to be suppressed"
        );

        assert!(
            action_rx.try_recv().is_err(),
            "expected no immediate retry action"
        );

        handle_codex_event(
            Event {
                id: "turn-complete".into(),
                msg: EventMsg::TurnComplete(TurnCompleteEvent {
                    turn_id: "turn-1".to_string(),
                    last_agent_message: None,
                }),
            },
            &mut recovery,
            &event_tx,
        );

        assert!(
            event_rx.try_recv().is_err(),
            "expected empty TurnComplete to be suppressed during retry streak"
        );
        assert_eq!(
            action_rx.try_recv().expect("expected RetryContinue action"),
            RecoveryAction::RetryContinue { attempt: 1 }
        );

        handle_codex_event(
            Event {
                id: "activity".into(),
                msg: EventMsg::AgentMessageDelta(AgentMessageDeltaEvent {
                    delta: "hello".to_string(),
                }),
            },
            &mut recovery,
            &event_tx,
        );

        let recovered = event_rx.try_recv().expect("expected recovered event");
        assert!(matches!(
            recovered.msg,
            EventMsg::PotterStreamRecoveryRecovered
        ));

        let forwarded = event_rx
            .try_recv()
            .expect("expected forwarded activity event");
        assert!(matches!(forwarded.msg, EventMsg::AgentMessageDelta(_)));
    }

    #[test]
    fn stream_recovery_ignores_retryable_error_before_first_turn_start() {
        let (event_tx, mut event_rx) = unbounded_channel::<Event>();
        let (action_tx, mut action_rx) = unbounded_channel::<RecoveryAction>();
        let mut recovery = StreamRecoveryContext {
            stream_recovery: PotterStreamRecovery::new(),
            recovery_action_tx: action_tx,
            pending_continue_retry: None,
            has_sent_turn_start: false,
            has_finished_round: false,
            last_turn_start_was_recovery_continue: false,
        };

        handle_codex_event(
            Event {
                id: "err".into(),
                msg: EventMsg::Error(retryable_error_event()),
            },
            &mut recovery,
            &event_tx,
        );

        let event = event_rx.try_recv().expect("expected forwarded error");
        assert!(matches!(event.msg, EventMsg::Error(_)));

        let round_finished = event_rx.try_recv().expect("expected round finished marker");
        assert!(matches!(
            round_finished.msg,
            EventMsg::PotterRoundFinished {
                outcome: PotterRoundOutcome::Fatal { .. }
            }
        ));
        assert!(action_rx.try_recv().is_err(), "expected no continue action");
    }

    #[test]
    fn stream_recovery_gives_up_after_retry_cap() {
        let (event_tx, mut event_rx) = unbounded_channel::<Event>();
        let (action_tx, mut action_rx) = unbounded_channel::<RecoveryAction>();
        let mut recovery = StreamRecoveryContext {
            stream_recovery: PotterStreamRecovery::new(),
            recovery_action_tx: action_tx,
            pending_continue_retry: None,
            has_sent_turn_start: true,
            has_finished_round: false,
            last_turn_start_was_recovery_continue: false,
        };

        let err = retryable_error_event();
        for _ in 0..10 {
            let Some(ContinueRetryDecision::Retry(_)) = recovery.stream_recovery.plan_retry(&err)
            else {
                panic!("expected retry plan while warming retry streak");
            };
        }

        handle_codex_event(
            Event {
                id: "err".into(),
                msg: EventMsg::Error(err),
            },
            &mut recovery,
            &event_tx,
        );

        let event = event_rx.try_recv().expect("expected injected event");
        let EventMsg::PotterStreamRecoveryGaveUp {
            error_message,
            attempts,
            max_attempts,
        } = event.msg
        else {
            panic!("expected PotterStreamRecoveryGaveUp, got: {:?}", event.msg);
        };
        assert!(error_message.contains("stream disconnected before completion"));
        assert_eq!((attempts, max_attempts), (10, 10));

        let round_finished = event_rx.try_recv().expect("expected round finished marker");
        assert!(matches!(
            round_finished.msg,
            EventMsg::PotterRoundFinished {
                outcome: PotterRoundOutcome::TaskFailed { .. }
            }
        ));

        assert!(
            event_rx.try_recv().is_err(),
            "expected Error to be suppressed after giving up"
        );
        assert!(
            action_rx.try_recv().is_err(),
            "expected no continue action after giving up"
        );
    }

    #[test]
    fn thread_rollback_failed_error_is_forwarded_and_ends_round() {
        let (event_tx, mut event_rx) = unbounded_channel::<Event>();
        let (action_tx, mut action_rx) = unbounded_channel::<RecoveryAction>();
        let mut recovery = StreamRecoveryContext {
            stream_recovery: PotterStreamRecovery::new(),
            recovery_action_tx: action_tx,
            pending_continue_retry: None,
            has_sent_turn_start: true,
            has_finished_round: false,
            last_turn_start_was_recovery_continue: false,
        };

        handle_codex_event(
            Event {
                id: "rollback-failed".into(),
                msg: EventMsg::Error(ErrorEvent {
                    message: "Cannot rollback while a turn is in progress.".to_string(),
                    codex_error_info: Some(CodexErrorInfo::ThreadRollbackFailed),
                }),
            },
            &mut recovery,
            &event_tx,
        );

        let forwarded = event_rx.try_recv().expect("expected forwarded error");
        assert!(matches!(forwarded.msg, EventMsg::Error(_)));

        let round_finished = event_rx.try_recv().expect("expected round finished marker");
        assert!(matches!(
            round_finished.msg,
            EventMsg::PotterRoundFinished {
                outcome: PotterRoundOutcome::Fatal { .. }
            }
        ));
        assert!(
            action_rx.try_recv().is_err(),
            "expected no recovery action from ThreadRollbackFailed"
        );
        assert!(recovery.has_finished_round, "round should end as fatal");
    }

    #[test]
    fn thread_rolled_back_event_is_suppressed() {
        let (event_tx, mut event_rx) = unbounded_channel::<Event>();
        let (action_tx, mut action_rx) = unbounded_channel::<RecoveryAction>();
        let mut recovery = StreamRecoveryContext {
            stream_recovery: PotterStreamRecovery::new(),
            recovery_action_tx: action_tx,
            pending_continue_retry: None,
            has_sent_turn_start: true,
            has_finished_round: false,
            last_turn_start_was_recovery_continue: false,
        };

        handle_codex_event(
            Event {
                id: "rolled-back".into(),
                msg: EventMsg::ThreadRolledBack(ThreadRolledBackEvent { num_turns: 1 }),
            },
            &mut recovery,
            &event_tx,
        );

        assert!(
            event_rx.try_recv().is_err(),
            "expected ThreadRolledBack to be suppressed"
        );
        assert!(
            action_rx.try_recv().is_err(),
            "expected no recovery action from ThreadRolledBack"
        );
        assert!(!recovery.has_finished_round, "round should continue");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::TurnCompleteEvent;
    use codex_protocol::user_input::UserInput;
    use tokio::sync::mpsc::unbounded_channel;
    use tokio::time::Duration;
    use tokio::time::timeout;

    #[cfg(unix)]
    #[tokio::test]
    async fn backend_allows_another_turn_after_turn_complete() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let codex_bin = temp.path().join("dummy-codex");
        let marker = temp.path().join("saw-second-turn-start");

        let script = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail

MARKER="{marker}"

if [[ "${{1:-}}" != "app-server" ]]; then
  echo "expected app-server, got: $*" >&2
  exit 1
fi

# initialize request
IFS= read -r _line
echo '{{"id":1,"result":{{}}}}'

# initialized notification
IFS= read -r _line

# thread/start request
IFS= read -r _line
echo '{{"id":2,"result":{{"thread":{{"id":"00000000-0000-0000-0000-000000000000","path":"rollout.jsonl"}},"model":"test-model","modelProvider":"test-provider","cwd":"project","approvalPolicy":"never","sandbox":{{"type":"readOnly"}},"reasoningEffort":null}}}}'

# first turn/start request
IFS= read -r _line
echo '{{"id":3,"result":{{}}}}'

# signal completion for the first turn
echo '{{"method":"codex/event/test","params":{{"id":"1","msg":{{"type":"turn_complete","last_agent_message":null}}}}}}'

# second turn/start request (should still be accepted)
IFS= read -r _line
touch "$MARKER"
echo '{{"id":4,"result":{{}}}}'

# Wait for the client to close stdin to request shutdown.
while IFS= read -r _line; do
  :
done
"#,
            marker = marker.display()
        );

        std::fs::write(&codex_bin, script).expect("write dummy codex");
        let mut perms = std::fs::metadata(&codex_bin)
            .expect("stat dummy codex")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&codex_bin, perms).expect("chmod dummy codex");

        let (event_tx, mut event_rx) = unbounded_channel::<Event>();
        let (fatal_exit_tx, _fatal_exit_rx) = unbounded_channel::<String>();

        let (op_tx, mut op_rx) = unbounded_channel::<Op>();
        let backend = tokio::spawn(async move {
            run_app_server_backend_inner(
                AppServerBackendConfig {
                    codex_bin: codex_bin.display().to_string(),
                    developer_instructions: None,
                    launch: AppServerLaunchConfig {
                        spawn_sandbox: None,
                        thread_sandbox: None,
                        bypass_approvals_and_sandbox: false,
                    },
                    codex_home: None,
                    thread_cwd: None,
                    resume_thread_id: None,
                },
                &mut op_rx,
                &event_tx,
                &fatal_exit_tx,
            )
            .await
        });

        op_tx
            .send(Op::UserInput {
                items: vec![UserInput::Text {
                    text: "hello".to_string(),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            })
            .expect("send user input");

        let saw_turn_complete = timeout(Duration::from_secs(5), async {
            let mut observed = false;
            while let Some(event) = event_rx.recv().await {
                if matches!(event.msg, EventMsg::TurnComplete(TurnCompleteEvent { .. })) {
                    observed = true;
                    break;
                }
            }
            observed
        })
        .await;
        assert_eq!(saw_turn_complete, Ok(true), "did not observe TurnComplete");

        op_tx
            .send(Op::UserInput {
                items: vec![UserInput::Text {
                    text: "Continue".to_string(),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            })
            .expect("send second user input");

        timeout(Duration::from_secs(5), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for dummy server marker");

        drop(op_tx);

        timeout(Duration::from_secs(5), backend)
            .await
            .expect("backend timed out")
            .expect("backend panicked")
            .expect("backend failed");

        assert!(
            marker.exists(),
            "dummy server did not observe second turn/start"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backend_stream_recovery_rolls_back_last_continue_turn() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let codex_bin = temp.path().join("dummy-codex");
        let marker = temp.path().join("saw-rollback-then-continue");

        let script = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail

MARKER="{marker}"

if [[ "${{1:-}}" != "app-server" ]]; then
  echo "expected app-server, got: $*" >&2
  exit 1
fi

# initialize request
IFS= read -r _line
echo '{{"id":1,"result":{{}}}}'

# initialized notification
IFS= read -r _line

# thread/start request
IFS= read -r _line
echo '{{"id":2,"result":{{"thread":{{"id":"00000000-0000-0000-0000-000000000000","path":"rollout.jsonl"}},"model":"test-model","modelProvider":"test-provider","cwd":"project","approvalPolicy":"never","sandbox":{{"type":"readOnly"}},"reasoningEffort":null}}}}'

# first turn/start request
IFS= read -r _line
echo '{{"id":3,"result":{{}}}}'

# signal a retryable stream error, followed by an empty completion
echo '{{"method":"codex/event/test","params":{{"id":"err-1","msg":{{"type":"error","message":"stream disconnected before completion: error sending request for url (...)"}}}}}}'
echo '{{"method":"codex/event/test","params":{{"id":"tc-1","msg":{{"type":"turn_complete","last_agent_message":null}}}}}}'

# second turn/start request (first automatic Continue)
IFS= read -r turn_start
echo "$turn_start" | grep -q '"method":"turn/start"' || {{
  echo "expected turn/start, got: $turn_start" >&2
  exit 1
}}
echo "$turn_start" | grep -q '"text":"Continue"' || {{
  echo "expected Continue prompt, got: $turn_start" >&2
  exit 1
}}
echo '{{"id":4,"result":{{}}}}'

# signal another retryable stream error (attempt 2)
echo '{{"method":"codex/event/test","params":{{"id":"err-2","msg":{{"type":"error","message":"stream disconnected before completion: error sending request for url (...)"}}}}}}'
echo '{{"method":"codex/event/test","params":{{"id":"tc-2","msg":{{"type":"turn_complete","last_agent_message":null}}}}}}'

# thread/rollback request should occur before the next Continue
IFS= read -r rollback
echo "$rollback" | grep -q '"method":"thread/rollback"' || {{
  echo "expected thread/rollback, got: $rollback" >&2
  exit 1
}}
echo "$rollback" | grep -q '"numTurns":1' || {{
  echo "expected numTurns=1, got: $rollback" >&2
  exit 1
}}
echo '{{"method":"codex/event/thread_rolled_back","params":{{"id":"rb-1","msg":{{"type":"thread_rolled_back","num_turns":1}}}}}}'
echo '{{"id":5,"result":{{"thread":{{"id":"00000000-0000-0000-0000-000000000000","path":"rollout.jsonl"}}}}}}'

# third turn/start request (automatic Continue after rollback)
IFS= read -r turn_start
echo "$turn_start" | grep -q '"method":"turn/start"' || {{
  echo "expected turn/start, got: $turn_start" >&2
  exit 1
}}
echo "$turn_start" | grep -q '"text":"Continue"' || {{
  echo "expected Continue prompt, got: $turn_start" >&2
  exit 1
}}
touch "$MARKER"
echo '{{"id":6,"result":{{}}}}'

# Wait for the client to close stdin to request shutdown.
while IFS= read -r _line; do
  :
done
"#,
            marker = marker.display()
        );

        std::fs::write(&codex_bin, script).expect("write dummy codex");
        let mut perms = std::fs::metadata(&codex_bin)
            .expect("stat dummy codex")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&codex_bin, perms).expect("chmod dummy codex");

        let (event_tx, _event_rx) = unbounded_channel::<Event>();
        let (fatal_exit_tx, _fatal_exit_rx) = unbounded_channel::<String>();

        let (op_tx, mut op_rx) = unbounded_channel::<Op>();
        let backend = tokio::spawn(async move {
            run_app_server_backend_inner(
                AppServerBackendConfig {
                    codex_bin: codex_bin.display().to_string(),
                    developer_instructions: None,
                    launch: AppServerLaunchConfig {
                        spawn_sandbox: None,
                        thread_sandbox: None,
                        bypass_approvals_and_sandbox: false,
                    },
                    codex_home: None,
                    thread_cwd: None,
                    resume_thread_id: None,
                },
                &mut op_rx,
                &event_tx,
                &fatal_exit_tx,
            )
            .await
        });

        op_tx
            .send(Op::UserInput {
                items: vec![UserInput::Text {
                    text: "hello".to_string(),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            })
            .expect("send user input");

        timeout(Duration::from_secs(10), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for dummy server marker");

        drop(op_tx);

        timeout(Duration::from_secs(5), backend)
            .await
            .expect("backend timed out")
            .expect("backend panicked")
            .expect("backend failed");

        assert!(
            marker.exists(),
            "dummy server did not observe rollback+continue"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backend_user_input_cancels_pending_stream_recovery_continue() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let codex_bin = temp.path().join("dummy-codex");
        let marker = temp.path().join("saw-manual-input-without-auto-continue");

        let script = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail

MARKER="{marker}"

if [[ "${{1:-}}" != "app-server" ]]; then
  echo "expected app-server, got: $*" >&2
  exit 1
fi

# initialize request
IFS= read -r _line
echo '{{"id":1,"result":{{}}}}'

# initialized notification
IFS= read -r _line

# thread/start request
IFS= read -r _line
echo '{{"id":2,"result":{{"thread":{{"id":"00000000-0000-0000-0000-000000000000","path":"rollout.jsonl"}},"model":"test-model","modelProvider":"test-provider","cwd":"project","approvalPolicy":"never","sandbox":{{"type":"readOnly"}},"reasoningEffort":null}}}}'

# first turn/start request
IFS= read -r _line
echo '{{"id":3,"result":{{}}}}'

# signal a retryable stream error, followed by an empty completion
echo '{{"method":"codex/event/test","params":{{"id":"err-1","msg":{{"type":"error","message":"stream disconnected before completion: error sending request for url (...)"}}}}}}'
echo '{{"method":"codex/event/test","params":{{"id":"tc-1","msg":{{"type":"turn_complete","last_agent_message":null}}}}}}'

# second turn/start request (first automatic Continue)
IFS= read -r turn_start
echo "$turn_start" | grep -q '"method":"turn/start"' || {{
  echo "expected turn/start, got: $turn_start" >&2
  exit 1
}}
echo "$turn_start" | grep -q '"text":"Continue"' || {{
  echo "expected Continue prompt, got: $turn_start" >&2
  exit 1
}}
echo '{{"id":4,"result":{{}}}}'

# signal another retryable stream error (attempt 2), which would normally schedule a rollback+Continue
echo '{{"method":"codex/event/test","params":{{"id":"err-2","msg":{{"type":"error","message":"stream disconnected before completion: error sending request for url (...)"}}}}}}'
echo '{{"method":"codex/event/test","params":{{"id":"tc-2","msg":{{"type":"turn_complete","last_agent_message":null}}}}}}'

# third turn/start request (manual user input should cancel the pending automatic Continue)
IFS= read -r turn_start
echo "$turn_start" | grep -q '"method":"turn/start"' || {{
  if echo "$turn_start" | grep -q '"method":"thread/rollback"'; then
    echo "unexpected thread/rollback before manual input: $turn_start" >&2
  else
    echo "expected manual turn/start, got: $turn_start" >&2
  fi
  exit 1
}}
echo "$turn_start" | grep -q '"text":"manual"' || {{
  echo "expected manual prompt, got: $turn_start" >&2
  exit 1
}}
echo '{{"id":5,"result":{{}}}}'

# Ensure the pending retry action does not fire after the manual input.
if IFS= read -r -t 2 unexpected; then
  echo "expected no further requests, got: $unexpected" >&2
  exit 1
fi

touch "$MARKER"

# Wait for the client to close stdin to request shutdown.
while IFS= read -r _line; do
  :
done
"#,
            marker = marker.display(),
        );

        std::fs::write(&codex_bin, script).expect("write dummy codex");
        let mut perms = std::fs::metadata(&codex_bin)
            .expect("stat dummy codex")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&codex_bin, perms).expect("chmod dummy codex");

        let (event_tx, mut event_rx) = unbounded_channel::<Event>();
        let (fatal_exit_tx, _fatal_exit_rx) = unbounded_channel::<String>();

        let (op_tx, mut op_rx) = unbounded_channel::<Op>();
        let backend = tokio::spawn(async move {
            run_app_server_backend_inner(
                AppServerBackendConfig {
                    codex_bin: codex_bin.display().to_string(),
                    developer_instructions: None,
                    launch: AppServerLaunchConfig {
                        spawn_sandbox: None,
                        thread_sandbox: None,
                        bypass_approvals_and_sandbox: false,
                    },
                    codex_home: None,
                    thread_cwd: None,
                    resume_thread_id: None,
                },
                &mut op_rx,
                &event_tx,
                &fatal_exit_tx,
            )
            .await
        });

        op_tx
            .send(Op::UserInput {
                items: vec![UserInput::Text {
                    text: "hello".to_string(),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            })
            .expect("send user input");

        let saw_second_attempt = timeout(Duration::from_secs(5), async {
            while let Some(event) = event_rx.recv().await {
                if let EventMsg::PotterStreamRecoveryUpdate { attempt, .. } = event.msg
                    && attempt == 2
                {
                    return true;
                }
            }
            false
        })
        .await;
        assert_eq!(
            saw_second_attempt,
            Ok(true),
            "did not observe stream recovery attempt 2"
        );

        op_tx
            .send(Op::UserInput {
                items: vec![UserInput::Text {
                    text: "manual".to_string(),
                    text_elements: Vec::new(),
                }],
                final_output_json_schema: None,
            })
            .expect("send manual input");

        timeout(Duration::from_secs(5), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for dummy server marker");

        drop(op_tx);

        timeout(Duration::from_secs(5), backend)
            .await
            .expect("backend timed out")
            .expect("backend panicked")
            .expect("backend failed");

        assert!(
            marker.exists(),
            "dummy server did not observe manual input without auto continue"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backend_exits_when_op_channel_is_closed() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let codex_bin = temp.path().join("dummy-codex");
        let marker = temp.path().join("saw-stdin-eof");

        let script = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail

MARKER="{marker}"

if [[ "${{1:-}}" != "--dangerously-bypass-approvals-and-sandbox" ]]; then
  echo "expected --dangerously-bypass-approvals-and-sandbox, got: $*" >&2
  exit 1
fi
if [[ "${{2:-}}" != "app-server" ]]; then
  echo "expected app-server, got: $*" >&2
  exit 1
fi

# Emit enough stderr output to fill a typical pipe buffer if the client isn't draining it.
dd if=/dev/zero bs=1 count=131072 1>&2 2>/dev/null

# initialize request
IFS= read -r _line
echo '{{"id":1,"result":{{}}}}'

# initialized notification
IFS= read -r _line

# thread/start request
IFS= read -r thread_start
echo "$thread_start" | grep -q '"sandbox":"danger-full-access"' || {{
  echo "expected sandbox=danger-full-access in thread/start, got: $thread_start" >&2
  exit 1
}}
echo '{{"id":2,"result":{{"thread":{{"id":"00000000-0000-0000-0000-000000000000","preview":"","modelProvider":"test-provider","createdAt":0,"updatedAt":0,"path":"rollout.jsonl","cwd":"project","cliVersion":"0.0.0","source":"appServer","gitInfo":null,"turns":[]}},"model":"test-model","modelProvider":"test-provider","cwd":"project","approvalPolicy":"never","sandbox":{{"type":"readOnly"}},"reasoningEffort":null}}}}'

# Wait for the client to close stdin to request shutdown.
while IFS= read -r _line; do
  :
done

touch "$MARKER"
"#,
            marker = marker.display()
        );

        std::fs::write(&codex_bin, script).expect("write dummy codex");
        let mut perms = std::fs::metadata(&codex_bin)
            .expect("stat dummy codex")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&codex_bin, perms).expect("chmod dummy codex");

        let (event_tx, _event_rx) = unbounded_channel::<Event>();
        let (fatal_exit_tx, _fatal_exit_rx) = unbounded_channel::<String>();

        let (op_tx, mut op_rx) = unbounded_channel::<Op>();
        drop(op_tx);

        timeout(
            Duration::from_secs(5),
            run_app_server_backend_inner(
                AppServerBackendConfig {
                    codex_bin: codex_bin.display().to_string(),
                    developer_instructions: None,
                    launch: AppServerLaunchConfig {
                        spawn_sandbox: None,
                        thread_sandbox: Some(
                            crate::app_server_protocol::SandboxMode::DangerFullAccess,
                        ),
                        bypass_approvals_and_sandbox: true,
                    },
                    codex_home: None,
                    thread_cwd: None,
                    resume_thread_id: None,
                },
                &mut op_rx,
                &event_tx,
                &fatal_exit_tx,
            ),
        )
        .await
        .expect("backend timed out")
        .expect("backend failed");

        assert!(marker.exists(), "dummy server did not observe stdin EOF");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backend_exits_when_op_channel_is_closed_workspace_write() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let codex_bin = temp.path().join("dummy-codex");
        let marker = temp.path().join("saw-stdin-eof");

        let script = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail

MARKER="{marker}"

        if [[ "${{1:-}}" != "--sandbox" ]]; then
  echo "expected --sandbox, got: $*" >&2
  exit 1
fi
if [[ "${{2:-}}" != "workspace-write" ]]; then
  echo "expected workspace-write, got: $*" >&2
  exit 1
fi
if [[ "${{3:-}}" != "app-server" ]]; then
  echo "expected app-server, got: $*" >&2
  exit 1
fi

# Emit enough stderr output to fill a typical pipe buffer if the client isn't draining it.
dd if=/dev/zero bs=1 count=131072 1>&2 2>/dev/null

# initialize request
IFS= read -r _line
echo '{{"id":1,"result":{{}}}}'

# initialized notification
IFS= read -r _line

# thread/start request
IFS= read -r thread_start
echo "$thread_start" | grep -q '"sandbox":"workspace-write"' || {{
  echo "expected sandbox=workspace-write in thread/start, got: $thread_start" >&2
  exit 1
}}
echo '{{"id":2,"result":{{"thread":{{"id":"00000000-0000-0000-0000-000000000000","preview":"","modelProvider":"test-provider","createdAt":0,"updatedAt":0,"path":"rollout.jsonl","cwd":"project","cliVersion":"0.0.0","source":"appServer","gitInfo":null,"turns":[]}},"model":"test-model","modelProvider":"test-provider","cwd":"project","approvalPolicy":"never","sandbox":{{"type":"workspaceWrite","writableRoots":[],"networkAccess":false,"excludeTmpdirEnvVar":false,"excludeSlashTmp":false}},"reasoningEffort":null}}}}'

# Wait for the client to close stdin to request shutdown.
while IFS= read -r _line; do
  :
done

touch "$MARKER"
"#,
            marker = marker.display()
        );

        std::fs::write(&codex_bin, script).expect("write dummy codex");
        let mut perms = std::fs::metadata(&codex_bin)
            .expect("stat dummy codex")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&codex_bin, perms).expect("chmod dummy codex");

        let (event_tx, _event_rx) = unbounded_channel::<Event>();
        let (fatal_exit_tx, _fatal_exit_rx) = unbounded_channel::<String>();

        let (op_tx, mut op_rx) = unbounded_channel::<Op>();
        drop(op_tx);

        timeout(
            Duration::from_secs(5),
            run_app_server_backend_inner(
                AppServerBackendConfig {
                    codex_bin: codex_bin.display().to_string(),
                    developer_instructions: None,
                    launch: AppServerLaunchConfig {
                        spawn_sandbox: Some(
                            crate::app_server_protocol::SandboxMode::WorkspaceWrite,
                        ),
                        thread_sandbox: Some(
                            crate::app_server_protocol::SandboxMode::WorkspaceWrite,
                        ),
                        bypass_approvals_and_sandbox: false,
                    },
                    codex_home: None,
                    thread_cwd: None,
                    resume_thread_id: None,
                },
                &mut op_rx,
                &event_tx,
                &fatal_exit_tx,
            ),
        )
        .await
        .expect("backend timed out")
        .expect("backend failed");

        assert!(marker.exists(), "dummy server did not observe stdin EOF");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backend_does_not_pass_sandbox_flag_for_default_mode() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let codex_bin = temp.path().join("dummy-codex");
        let marker = temp.path().join("saw-stdin-eof");

        let script = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail

MARKER="{marker}"

if [[ "${{1:-}}" != "app-server" ]]; then
  echo "expected app-server, got: $*" >&2
  exit 1
fi

# initialize request
IFS= read -r _line
echo '{{"id":1,"result":{{}}}}'

# initialized notification
IFS= read -r _line

# thread/start request
IFS= read -r thread_start
echo "$thread_start" | grep -q '"sandbox":null' || {{
  echo "expected sandbox=null in thread/start, got: $thread_start" >&2
  exit 1
}}
echo '{{"id":2,"result":{{"thread":{{"id":"00000000-0000-0000-0000-000000000000","preview":"","modelProvider":"test-provider","createdAt":0,"updatedAt":0,"path":"rollout.jsonl","cwd":"project","cliVersion":"0.0.0","source":"appServer","gitInfo":null,"turns":[]}},"model":"test-model","modelProvider":"test-provider","cwd":"project","approvalPolicy":"never","sandbox":{{"type":"readOnly"}},"reasoningEffort":null}}}}'

# Wait for the client to close stdin to request shutdown.
while IFS= read -r _line; do
  :
done

touch "$MARKER"
"#,
            marker = marker.display()
        );

        std::fs::write(&codex_bin, script).expect("write dummy codex");
        let mut perms = std::fs::metadata(&codex_bin)
            .expect("stat dummy codex")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&codex_bin, perms).expect("chmod dummy codex");

        let (event_tx, _event_rx) = unbounded_channel::<Event>();
        let (fatal_exit_tx, _fatal_exit_rx) = unbounded_channel::<String>();

        let (op_tx, mut op_rx) = unbounded_channel::<Op>();
        drop(op_tx);

        timeout(
            Duration::from_secs(5),
            run_app_server_backend_inner(
                AppServerBackendConfig {
                    codex_bin: codex_bin.display().to_string(),
                    developer_instructions: None,
                    launch: AppServerLaunchConfig {
                        spawn_sandbox: None,
                        thread_sandbox: None,
                        bypass_approvals_and_sandbox: false,
                    },
                    codex_home: None,
                    thread_cwd: None,
                    resume_thread_id: None,
                },
                &mut op_rx,
                &event_tx,
                &fatal_exit_tx,
            ),
        )
        .await
        .expect("backend timed out")
        .expect("backend failed");

        assert!(marker.exists(), "dummy server did not observe stdin EOF");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backend_sets_codex_home_env_when_provided() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let codex_bin = temp.path().join("dummy-codex");
        let marker = temp.path().join("saw-stdin-eof");
        let codex_home = temp.path().join("codex-home");
        std::fs::create_dir_all(&codex_home).expect("create codex home");

        let script = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail

MARKER="{marker}"
EXPECTED_CODEX_HOME="{codex_home}"

if [[ "${{1:-}}" != "app-server" ]]; then
  echo "expected app-server, got: $*" >&2
  exit 1
fi

if [[ "${{CODEX_HOME:-}}" != "$EXPECTED_CODEX_HOME" ]]; then
  echo "expected CODEX_HOME=$EXPECTED_CODEX_HOME, got: ${{CODEX_HOME:-}}" >&2
  exit 1
fi

# initialize request
IFS= read -r _line
echo '{{"id":1,"result":{{}}}}'

# initialized notification
IFS= read -r _line

# thread/start request
IFS= read -r thread_start
if echo "$thread_start" | grep -Fq '"codex_home"'; then
  echo "did not expect codex_home in thread/start config, got: $thread_start" >&2
  exit 1
fi
echo '{{"id":2,"result":{{"thread":{{"id":"00000000-0000-0000-0000-000000000000","preview":"","modelProvider":"test-provider","createdAt":0,"updatedAt":0,"path":"rollout.jsonl","cwd":"project","cliVersion":"0.0.0","source":"appServer","gitInfo":null,"turns":[]}},"model":"test-model","modelProvider":"test-provider","cwd":"project","approvalPolicy":"never","sandbox":{{"type":"readOnly"}},"reasoningEffort":null}}}}'

# Wait for the client to close stdin to request shutdown.
while IFS= read -r _line; do
  :
done

touch "$MARKER"
"#,
            marker = marker.display(),
            codex_home = codex_home.display(),
        );

        std::fs::write(&codex_bin, script).expect("write dummy codex");
        let mut perms = std::fs::metadata(&codex_bin)
            .expect("stat dummy codex")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&codex_bin, perms).expect("chmod dummy codex");

        let (event_tx, _event_rx) = unbounded_channel::<Event>();
        let (fatal_exit_tx, _fatal_exit_rx) = unbounded_channel::<String>();

        let (op_tx, mut op_rx) = unbounded_channel::<Op>();
        drop(op_tx);

        timeout(
            Duration::from_secs(5),
            run_app_server_backend_inner(
                AppServerBackendConfig {
                    codex_bin: codex_bin.display().to_string(),
                    developer_instructions: None,
                    launch: AppServerLaunchConfig {
                        spawn_sandbox: None,
                        thread_sandbox: None,
                        bypass_approvals_and_sandbox: false,
                    },
                    codex_home: Some(codex_home),
                    thread_cwd: None,
                    resume_thread_id: None,
                },
                &mut op_rx,
                &event_tx,
                &fatal_exit_tx,
            ),
        )
        .await
        .expect("backend timed out")
        .expect("backend failed");

        assert!(marker.exists(), "dummy server did not observe stdin EOF");
    }
}
