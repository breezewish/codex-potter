# Progress Files and Knowledge Base (`.codexpotter/`)

`codex-potter` is designed around "filesystem as memory". Each session has a durable progress file
that the agent reads and updates every round, plus an optional scratch knowledge base (KB)
directory used to record intermediate findings.

This page documents the conventions and how they are used by the runner.

## Progress file (`.codexpotter/projects/.../MAIN.md`)

### Location and naming

Created by the CLI in the project working directory:

- `.codexpotter/projects/YYYYMMDD_N/MAIN.md`

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
