# Progress Files and Knowledge Base (`.codexpotter/`)

`codex-potter` is designed around "filesystem as memory". Each session has a durable progress file
that the agent reads and updates every round, plus an optional scratch knowledge base (KB)
directory used to record intermediate findings.

This page documents the conventions and how they are used by the runner.

## Progress file (`.codexpotter/projects/.../MAIN.md`)

### Location and naming

Created by the CLI in the project working directory:

- `.codexpotter/projects/YYYY/MM/DD/N/MAIN.md`

The template is embedded in the binary:

- `cli/prompts/project_main.md`

### Structure

The file has two parts:

1. YAML front matter (between `---` markers)
2. Markdown sections used by the workflow

Canonical section names used by the workflow template:

- `# Overall Goal`
- `## In Progress`
- `## Todo`
- `## Done`

### Front matter fields

The workflow template defines:

- `status`: `initial` | `open` | `skip`
  - **Used by the workflow prompt** to decide whether to plan vs execute.
  - **Not currently parsed by the `codex-potter` runner** (the CLI does not read it).
- `short_title`: short human-readable title for the session
  - Set during the first round (when `status: initial`).
  - **Not currently parsed by the runner**.
- `git_commit`: git commit SHA captured when the session is created
  - Empty when the working directory is not a git repo (or HEAD cannot be resolved).
  - **Not currently parsed by the runner**.
- `git_branch`: git branch name captured when the session is created
  - Empty when not on a branch (detached HEAD) or when the working directory is not a git repo.
  - **Not currently parsed by the runner**.
- `finite_incantatem`: `true` | `false`
  - **The only field currently read by the runner.**
  - When `true`, the CLI stops running additional rounds for the current session
    (`cli/src/project.rs`: `progress_file_has_finite_incantatem_true`).
  - Queued sessions (queued user prompts) continue normally.

### How the file is used at runtime

- The CLI injects the progress file *relative path* into the developer prompt
  (`cli/src/project.rs`: `render_developer_prompt` + `cli/prompts/developer_prompt.md`).
- Each round uses a fixed user prompt (`cli/prompts/prompt.md`) that instructs the agent to
  continue working according to the workflow.
- The agent is expected to:
  - keep tasks updated by moving items between `Todo` / `In Progress` / `Done`
  - commit code changes after completing tasks (but never commit `.codexpotter/`)
  - avoid referencing file line numbers in docs

## Potter rollout log (`potter-rollout.jsonl`)

CodexPotter writes an additional append-only JSONL log in each project directory:

- `.codexpotter/projects/YYYY/MM/DD/N/potter-rollout.jsonl`

This file is the durable index that links the project to upstream app-server rollouts and captures
Potter-specific session/round boundary events. It is used by `codex-potter resume` to replay
history and to continue iterating on the same project.

The log is intentionally minimal: it does **not** duplicate upstream rollout content. Instead, it
records `(thread_id, rollout_path)` for each round and relies on the upstream `rollout-*.jsonl` as
the source of truth for persisted `EventMsg` items.

### Format

Each line is a single JSON object (append-only). The schema is a tagged enum with `type`:

- `session_started`
  - `user_message` (optional): the original user prompt text (stored verbatim for replay).
  - `user_prompt_file`: the progress file path captured at session start.
- `round_started`
  - `current`: 1-based round counter shown in the UI.
  - `total`: round budget shown in the UI for that session segment.
- `round_configured`
  - `thread_id`: upstream app-server session/thread id.
  - `rollout_path`: path to the upstream rollout file (recorded as an absolute path when possible).
  - `rollout_path_raw` / `rollout_base_dir` (optional): debugging fields populated when path
    canonicalization fails.
- `session_succeeded`
  - `rounds`: number of rounds recorded for the overall project (used for summary rendering).
  - `duration_secs`: wall-clock elapsed time for the project.
  - `user_prompt_file`: progress file path.
  - `git_commit_start` / `git_commit_end`: git commit SHAs captured for the summary.
- `round_finished`
  - `outcome`: `completed` | `user_requested` | `task_failed` | `fatal` (payload matches the
    `PotterRoundOutcome` schema in `codex-protocol`).

### Compatibility

Projects created before `potter-rollout.jsonl` was introduced cannot currently be resumed; `resume`
fails fast with an "unsupported project" error.

## Knowledge base (gitignored scratch directory)

### Purpose

The KB is an intentionally gitignored scratch directory used to capture intermediate findings while
exploring the codebase:

- module entry points and responsibilities
- tricky behavior and edge cases
- upstream vs potter divergences and where they live

It acts as a "working memory" across rounds, while the wiki pages under `docs/wiki/` are the
durable knowledge that should be committed.

### Conventions

- Keep a lightweight index (one-line summaries) for each KB note so it stays navigable.
- Treat KB notes as potentially stale: **the code is the source of truth**.
- Never commit anything under `.codexpotter/` (it is gitignored by design).
