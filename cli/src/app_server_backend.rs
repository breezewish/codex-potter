use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use crate::DONE_MARKER;
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
use crate::app_server_protocol::ThreadStartParams;
use crate::app_server_protocol::ThreadStartResponse;
use crate::app_server_protocol::TurnStartParams;
use crate::app_server_protocol::TurnStartResponse;
use crate::app_server_protocol::UserInput as ApiUserInput;
use anyhow::Context;
use codex_protocol::ThreadId;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::protocol::SessionConfiguredEvent;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendOutcome {
    pub done_marker_seen: bool,
}

#[derive(Debug, Default)]
struct BackendMessageState {
    turn_complete_seen: bool,
    done_marker_seen: bool,
    saw_agent_delta: bool,
    agent_message_buf: String,
}

pub async fn run_app_server_backend(
    codex_bin: String,
    developer_instructions: Option<String>,
    launch: AppServerLaunchConfig,
    codex_home: Option<PathBuf>,
    mut op_rx: UnboundedReceiver<Op>,
    event_tx: UnboundedSender<Event>,
    fatal_exit_tx: UnboundedSender<String>,
) -> anyhow::Result<BackendOutcome> {
    match run_app_server_backend_inner(
        codex_bin,
        developer_instructions,
        launch,
        codex_home,
        &mut op_rx,
        &event_tx,
        &fatal_exit_tx,
    )
    .await
    {
        Ok(outcome) => Ok(outcome),
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
            Ok(BackendOutcome {
                done_marker_seen: false,
            })
        }
    }
}

async fn run_app_server_backend_inner(
    codex_bin: String,
    developer_instructions: Option<String>,
    launch: AppServerLaunchConfig,
    codex_home: Option<PathBuf>,
    op_rx: &mut UnboundedReceiver<Op>,
    event_tx: &UnboundedSender<Event>,
    fatal_exit_tx: &UnboundedSender<String>,
) -> anyhow::Result<BackendOutcome> {
    let (mut child, stdin, stdout, stderr) = spawn_app_server(&codex_bin, launch).await?;
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
    let mut message_state = BackendMessageState::default();

    let result = async {
        initialize_app_server(
            stdin
                .as_mut()
                .context("codex app-server stdin unavailable")?,
            &mut lines,
            &mut next_id,
            event_tx,
            &mut message_state,
        )
        .await?;

        let thread_start = thread_start(
            stdin
                .as_mut()
                .context("codex app-server stdin unavailable")?,
            &mut lines,
            &mut next_id,
            ThreadStartSettings {
                developer_instructions,
                sandbox_mode: launch.thread_sandbox,
                codex_home,
            },
            event_tx,
            &mut message_state,
        )
        .await?;
        let thread_id = thread_start.thread.id.clone();

        let session_configured = synthesize_session_configured(&thread_start)?;
        let _ = event_tx.send(Event {
            id: "".to_string(),
            msg: EventMsg::SessionConfigured(session_configured),
        });

        loop {
            tokio::select! {
                maybe_op = op_rx.recv(), if !message_state.turn_complete_seen => {
                    let Some(op) = maybe_op else {
                        message_state.turn_complete_seen = true;
                        stdin.take();
                        continue;
                    };
                    handle_op(
                        &thread_id,
                        op,
                        stdin.as_mut().context("codex app-server stdin unavailable")?,
                        &mut lines,
                        &mut next_id,
                        event_tx,
                        &mut message_state,
                    )
                    .await?;
                }
                maybe_line = lines.next_line() => {
                    let Some(line) = maybe_line? else {
                        break;
                    };
                    let msg: JSONRPCMessage = serde_json::from_str(&line)
                        .with_context(|| format!("failed to decode app-server message: {line}"))?;
                    if handle_app_server_message(
                        msg,
                        &mut stdin,
                        event_tx,
                        &mut message_state,
                    )
                    .await?
                    {
                        // turn completed; request the server exit by closing stdin.
                        stdin.take();
                    }
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

    // If the backend finishes without a TurnComplete, ensure the UI can still exit.
    if !message_state.turn_complete_seen {
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

    Ok(BackendOutcome {
        done_marker_seen: message_state.done_marker_seen,
    })
}

async fn spawn_app_server(
    codex_bin: &str,
    launch: AppServerLaunchConfig,
) -> anyhow::Result<(Child, ChildStdin, ChildStdout, ChildStderr)> {
    let mut cmd = Command::new(codex_bin);
    cmd.kill_on_drop(true);

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
    event_tx: &UnboundedSender<Event>,
    message_state: &mut BackendMessageState,
) -> anyhow::Result<()> {
    let request_id = next_request_id(next_id);
    let request = ClientRequest::Initialize {
        request_id: request_id.clone(),
        params: InitializeParams {
            client_info: ClientInfo {
                name: "codex-potter".to_string(),
                title: Some("codex-potter".to_string()),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        },
    };
    send_message(stdin, &request).await?;
    let _response = read_until_response(stdin, lines, request_id, event_tx, message_state).await?;

    send_message(stdin, &ClientNotification::Initialized).await?;
    Ok(())
}

struct ThreadStartSettings {
    developer_instructions: Option<String>,
    sandbox_mode: Option<crate::app_server_protocol::SandboxMode>,
    codex_home: Option<PathBuf>,
}

async fn thread_start(
    stdin: &mut ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    next_id: &mut i64,
    settings: ThreadStartSettings,
    event_tx: &UnboundedSender<Event>,
    message_state: &mut BackendMessageState,
) -> anyhow::Result<ThreadStartResponse> {
    let ThreadStartSettings {
        developer_instructions,
        sandbox_mode,
        codex_home,
    } = settings;
    let request_id = next_request_id(next_id);
    let config = codex_home.map(|codex_home| {
        let mut config = std::collections::HashMap::new();
        config.insert(
            "codex_home".to_string(),
            serde_json::Value::String(codex_home.to_string_lossy().into_owned()),
        );
        config
    });
    let request = ClientRequest::ThreadStart {
        request_id: request_id.clone(),
        params: ThreadStartParams {
            model: None,
            model_provider: None,
            cwd: None,
            approval_policy: Some(crate::app_server_protocol::AskForApproval::Never),
            sandbox: sandbox_mode,
            config,
            base_instructions: None,
            developer_instructions,
            experimental_raw_events: false,
        },
    };
    send_message(stdin, &request).await?;
    let response = read_until_response(stdin, lines, request_id, event_tx, message_state).await?;
    serde_json::from_value(response.result).context("decode thread/start response")
}

async fn handle_op(
    thread_id: &str,
    op: Op,
    stdin: &mut ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    next_id: &mut i64,
    event_tx: &UnboundedSender<Event>,
    message_state: &mut BackendMessageState,
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
                read_until_response(stdin, lines, request_id, event_tx, message_state).await?;
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
    event_tx: &UnboundedSender<Event>,
    message_state: &mut BackendMessageState,
) -> anyhow::Result<bool> {
    match msg {
        JSONRPCMessage::Notification(notification) => {
            handle_codex_event_notification(
                &notification.method,
                notification.params,
                event_tx,
                message_state,
            )?;
        }
        JSONRPCMessage::Request(request) => {
            if let Some(stdin) = stdin.as_mut() {
                handle_server_request(stdin, request).await?;
            }
        }
        JSONRPCMessage::Response(_) | JSONRPCMessage::Error(_) => {}
    }

    Ok(message_state.turn_complete_seen)
}

fn handle_codex_event_notification(
    method: &str,
    params: Option<serde_json::Value>,
    event_tx: &UnboundedSender<Event>,
    message_state: &mut BackendMessageState,
) -> anyhow::Result<()> {
    if !method.starts_with("codex/event/") {
        return Ok(());
    }
    let Some(params) = params else {
        return Ok(());
    };

    let event: Event = serde_json::from_value(params)?;
    match &event.msg {
        EventMsg::AgentMessageDelta(ev) => {
            message_state.saw_agent_delta = true;
            message_state.agent_message_buf.push_str(&ev.delta);
        }
        EventMsg::AgentMessage(ev) => {
            if !message_state.saw_agent_delta {
                message_state.agent_message_buf = ev.message.clone();
            }
        }
        EventMsg::TurnComplete(ev) => {
            if let Some(last) = &ev.last_agent_message {
                if last.contains(DONE_MARKER) {
                    message_state.done_marker_seen = true;
                }
            } else if message_state.agent_message_buf.contains(DONE_MARKER) {
                message_state.done_marker_seen = true;
            }
        }
        _ => {}
    }
    if matches!(
        event.msg,
        EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_) | EventMsg::Error(_)
    ) {
        message_state.turn_complete_seen = true;
    }
    let _ = event_tx.send(event);
    Ok(())
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
    event_tx: &UnboundedSender<Event>,
    message_state: &mut BackendMessageState,
) -> anyhow::Result<JSONRPCResponse> {
    loop {
        let Some(line) = lines.next_line().await? else {
            anyhow::bail!("app-server stdout closed while waiting for response {request_id:?}");
        };
        let msg: JSONRPCMessage =
            serde_json::from_str(&line).with_context(|| format!("decode json-rpc: {line}"))?;

        match msg {
            JSONRPCMessage::Response(response) if response.id == request_id => return Ok(response),
            JSONRPCMessage::Error(err) if err.id == request_id => {
                anyhow::bail!(
                    "app-server returned error for {request_id:?}: {:?}",
                    err.error
                );
            }
            JSONRPCMessage::Notification(notification) => {
                handle_codex_event_notification(
                    &notification.method,
                    notification.params,
                    event_tx,
                    message_state,
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
    thread_start: &ThreadStartResponse,
) -> anyhow::Result<SessionConfiguredEvent> {
    let thread_id =
        ThreadId::from_string(thread_start.thread.id.as_str()).context("parse thread id")?;

    Ok(SessionConfiguredEvent {
        session_id: thread_id,
        forked_from_id: None,
        model: thread_start.model.clone(),
        model_provider_id: thread_start.model_provider.clone(),
        cwd: thread_start.cwd.clone(),
        reasoning_effort: thread_start.reasoning_effort,
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        rollout_path: thread_start.thread.path.clone(),
    })
}

fn next_request_id(next_id: &mut i64) -> RequestId {
    let id = *next_id;
    *next_id += 1;
    RequestId::Integer(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::AgentMessageDeltaEvent;
    use codex_protocol::protocol::TurnCompleteEvent;
    use tokio::sync::mpsc::unbounded_channel;
    use tokio::time::Duration;
    use tokio::time::timeout;

    #[test]
    fn done_marker_seen_from_turn_complete_last_agent_message() {
        let (event_tx, _event_rx) = unbounded_channel::<Event>();
        let mut state = BackendMessageState::default();

        let event = Event {
            id: "1".to_string(),
            msg: EventMsg::TurnComplete(TurnCompleteEvent {
                last_agent_message: Some(format!("ok {DONE_MARKER}")),
            }),
        };
        handle_codex_event_notification(
            "codex/event/test",
            Some(serde_json::to_value(event).expect("serialize event")),
            &event_tx,
            &mut state,
        )
        .expect("handle event");

        assert!(state.done_marker_seen);
        assert!(state.turn_complete_seen);
    }

    #[test]
    fn done_marker_seen_from_agent_delta_buffer_when_turn_complete_has_no_last_message() {
        let (event_tx, _event_rx) = unbounded_channel::<Event>();
        let mut state = BackendMessageState::default();

        let delta_event = Event {
            id: "1".to_string(),
            msg: EventMsg::AgentMessageDelta(AgentMessageDeltaEvent {
                delta: format!("hello {DONE_MARKER} world"),
            }),
        };
        handle_codex_event_notification(
            "codex/event/test",
            Some(serde_json::to_value(delta_event).expect("serialize delta event")),
            &event_tx,
            &mut state,
        )
        .expect("handle delta");

        let complete_event = Event {
            id: "1".to_string(),
            msg: EventMsg::TurnComplete(TurnCompleteEvent {
                last_agent_message: None,
            }),
        };
        handle_codex_event_notification(
            "codex/event/test",
            Some(serde_json::to_value(complete_event).expect("serialize complete event")),
            &event_tx,
            &mut state,
        )
        .expect("handle complete");

        assert!(state.done_marker_seen);
        assert!(state.turn_complete_seen);
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
                codex_bin.display().to_string(),
                None,
                AppServerLaunchConfig {
                    spawn_sandbox: None,
                    thread_sandbox: Some(crate::app_server_protocol::SandboxMode::DangerFullAccess),
                    bypass_approvals_and_sandbox: true,
                },
                None,
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
                codex_bin.display().to_string(),
                None,
                AppServerLaunchConfig {
                    spawn_sandbox: Some(crate::app_server_protocol::SandboxMode::WorkspaceWrite),
                    thread_sandbox: Some(crate::app_server_protocol::SandboxMode::WorkspaceWrite),
                    bypass_approvals_and_sandbox: false,
                },
                None,
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
                codex_bin.display().to_string(),
                None,
                AppServerLaunchConfig {
                    spawn_sandbox: None,
                    thread_sandbox: None,
                    bypass_approvals_and_sandbox: false,
                },
                None,
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
    async fn backend_includes_codex_home_config_when_provided() {
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
CODEX_HOME="{codex_home}"

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
echo "$thread_start" | grep -Fq "\"codex_home\":\"$CODEX_HOME\"" || {{
  echo "expected codex_home in thread/start config, got: $thread_start" >&2
  exit 1
}}
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
                codex_bin.display().to_string(),
                None,
                AppServerLaunchConfig {
                    spawn_sandbox: None,
                    thread_sandbox: None,
                    bypass_approvals_and_sandbox: false,
                },
                Some(codex_home),
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
