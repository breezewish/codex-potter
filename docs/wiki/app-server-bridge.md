# App-Server JSON-RPC Bridge

`codex-potter` does not embed Codex "core" in-process. Instead, each round spawns an external
upstream `codex` process in **app-server** mode and talks to it over stdin/stdout.

This page documents the bridge implementation in `cli/src/app_server_backend.rs` and the local
schema copy in `cli/src/app_server_protocol/`.

## Ownership

- **Potter-specific**: the bridge task (`cli/src/app_server_backend.rs`) is owned by this repo.
  It exists because `codex-potter` is non-interactive and needs a stable, bounded, programmatic
  way to drive `codex`.
- **Upstream-derived**: JSON-RPC method names, payload shapes, and most semantics come from the
  upstream `codex` CLI app-server. The protocol structs under `cli/src/app_server_protocol/` are a
  trimmed local mirror; keep them aligned with upstream when updating.

## Transport model (stdin/stdout, line-delimited JSON)

The bridge uses a simple newline-delimited JSON transport:

- one JSON object per line on stdout
- client writes one JSON object per line to stdin

This is intentionally **not** full JSON-RPC 2.0 (we do not send or expect the `"jsonrpc": "2.0"`
field). See `cli/src/app_server_protocol/jsonrpc_lite.rs`.

## Process + sandbox model

The bridge distinguishes between:

- **spawn sandbox**: the sandbox flag passed to the `codex` process itself
- **thread sandbox**: the sandbox value requested during `thread/start`

Both are derived from the CLI flags via `AppServerLaunchConfig::from_cli(...)`:

- When `--sandbox <mode>` is provided (and not `default`), `codex-potter` passes `--sandbox <mode>`
  to the process and also requests the same sandbox at `thread/start`.
- When `--yolo` / `--dangerously-bypass-approvals-and-sandbox` is used, `codex-potter`:
  - launches the app-server with the upstream bypass flag, and
  - requests `sandbox: danger-full-access` at `thread/start`.

## Lifecycle: initialize → thread/start → turn/start

The bridge runs one thread and one turn per round:

1. Spawn `codex app-server`.
2. Initialize the server protocol handshake.
3. Start a fresh thread (injecting developer instructions).
4. Start the turn when the UI submits `Op::UserInput`.
5. Forward streamed events (`codex/event/*`) to the TUI until the turn completes.

### 1) Spawn (`codex … app-server`)

Entry point: `spawn_app_server(...)` in `cli/src/app_server_backend.rs`.

It runs:

- `codex [--dangerously-bypass-approvals-and-sandbox] [--sandbox <mode>] app-server`

and captures stdin/stdout/stderr pipes.

### 2) Initialize handshake (`initialize` + `initialized`)

Entry point: `initialize_app_server(...)`.

Sequence:

1. Send a request: `initialize` (client identifies as `codex-potter`)
2. Wait for the matching response
3. Send a notification: `initialized`

The typed request wrapper lives in `cli/src/app_server_protocol/protocol/common.rs`:

- `ClientRequest::Initialize` uses **v1** payloads (`protocol/v1.rs`)
- `ClientNotification::Initialized` is a notification without `params`

### 3) Start a thread (`thread/start`)

Entry point: `thread_start(...)`.

Key behavior:

- Always requests `approvalPolicy: "never"` for the thread.
- Optionally requests a sandbox mode (`sandbox`) derived from CLI flags.
- Injects the developer prompt as `developerInstructions`.
- Optionally sets `config["codex_home"]` (the "codex-compat" home maintained by
  `cli/src/codex_compat.rs`).

Note: `thread/start` is modeled as a **v2** payload (`protocol/v2.rs`), even though `initialize`
uses v1.

After parsing the `ThreadStartResponse`, the bridge synthesizes a
`codex_protocol::protocol::EventMsg::SessionConfigured` event and sends it to the UI so the
renderer can show model/sandbox metadata consistently.

### 4) Start a turn (`turn/start`)

Entry point: `handle_op(...)` for `Op::UserInput`.

The TUI submits `Op::UserInput { items, final_output_json_schema }` which the bridge converts to a
`turn/start` request:

- thread id: from `thread/start` response
- input items: converted into app-server `UserInput` values (`protocol/v2.rs`)
- output schema: forwarded as `outputSchema`

Other `Op` variants are intentionally ignored in potter mode:

- `Op::Interrupt`: the render-only runner does not track a turn id, so `turn/interrupt` cannot be
  called.
- `Op::GetHistoryEntryRequest`: prompt history is stored locally by `codex-potter` and not fetched
  from the app-server.

## Event forwarding and "turn complete" detection

The app-server emits notifications for many methods; `codex-potter` only forwards
`codex/event/*` notifications.

Entry point: `handle_codex_event_notification(...)`.

Rules:

- Only methods starting with `codex/event/` are decoded into `codex_protocol::protocol::Event` and
  forwarded to the UI.
- Seeing any of these event messages marks the turn as completed:
  - `EventMsg::TurnComplete`
  - `EventMsg::TurnAborted`
  - `EventMsg::Error`

After the turn completes, the bridge closes stdin to request the app-server process exit.

## Approval auto-accept (non-interactive safety)

Even with `approvalPolicy: "never"`, the app-server can still send server-initiated requests (for
example when it wants an approval decision).

Because `codex-potter` is non-interactive, it must not block waiting for a user decision. The
bridge therefore auto-accepts known approval requests in `handle_server_request(...)`:

- `item/commandExecution/requestApproval` → accept
- `item/fileChange/requestApproval` → accept
- `applyPatch` → approved
- `execCommand` → approved

If the app-server sends a request that is not modeled in `ServerRequest`, the bridge responds with
a JSON-RPC error (`-32601`) rather than hanging.

## Robustness details

### Avoiding deadlocks while waiting for responses

When the bridge is waiting for a specific response id (for example `thread/start`), it still
processes:

- `codex/event/*` notifications (forwarded to the UI), and
- server-initiated approval requests (auto-accepted)

This is implemented in `read_until_response(...)` and prevents "response wait" from blocking the
entire session when the server emits interleaved messages.

### Capturing stderr without hanging

The bridge drains stderr in a background task and keeps a bounded capture (currently 32 KiB). On
error, the captured stderr is appended to the failure message so the TUI can surface actionable
context.
