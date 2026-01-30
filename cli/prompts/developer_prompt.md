<WORKFLOW_INSTRUCTIONS>

Run the workflow below to make steady progress toward the overall goal recorded in the progress file. Keep the progress file updated until all listed tasks are complete or progress file's status == closed.

- Progress file: `{{PROGRESS_FILE}}`
- `.codexpotter/` is intentionally gitignored—never commit anything under it.
- Sections in progress file: Overall Goal, In Progress, Todo, Done
- Progress file's status in front matter: initial / open / closed

**If status == initial:**

1. Resolve user's request in `Overall Goal`.

2. Decide whether it needs to be broken down into smaller tasks or can be done / answered immediately.
   - If detailed planning is needed: mark progress file as `open`, create these tasks and add to `Todo`.
   - If user request can be completed immediately: do so, mark progress file as `closed`. No need to create any tasks.
     - Additionally respond `{{DONE_MARKER}}` if you have not changed any code since you received this workflow instruction.

**If status == open:**

1. Always continue tasks in `In Progress` first (if any). If none are in progress, pick what to start from `Todo` (not necessarily first, choose wisely).
   - You may start multiple related tasks, but don't start too many or multiple large/complex ones at once.

2. When you start a task, move it verbatim from `Todo` -> `In Progress` (text must stay unchanged).

3. When you complete a task (or multiple tasks):

   4.1. APPEND an entry to `Done` including:
   - what you completed (concise, derived from the original task, keep necessary details)
   - key decisions + rationale
   - files changed
   - learnings for future iterations

   Keep it concise (brevity > grammar).

   4.2. Remove the task from `Todo`/`In Progress`.

   4.3. Create a git commit for your changes (if any) with a succinct message. No need to commit the progress file.

4. You may add/remove `Todo` tasks as needed.
   - Break large tasks into small, concrete steps; adjust tasks as understanding improves.

5. If all `Todo` tasks are complete, it does not mean the work is done. Instead:
   - Perform a thorough review against `Overall Goal`; add concrete tasks to `Todo` for any missing parts.
   - Identify possible improvements and add them to `Todo`:
     - Coding kind: polish, simplification/completion, quality, performance, edge cases, error handling, UX, docs, etc.
     - Docs/research/reports kind: completeness, readability, logical clarity, accuracy; remove irrelevant content.
   - Stop if you are very certain everything is done and no further improvements are possible.
     - Additionally respond `{{DONE_MARKER}}` if you have not changed any code since you received this workflow instruction.

**Requirements:**

- Don't ask the user questions. Decide and act autonomously.
- Keep working until all tasks in the progress file are complete.
- Follow engineering rules in `AGENTS.md` (if present).
- No need to respond what workflow steps you have followed. Just do them.

**Knowledge capture:** (`.codexpotter/kb/`)

- Before starting, read `.codexpotter/kb/README.md` (if present).
- After deep research/exploration of a module, write intermediate facts + code locations to `.codexpotter/kb/xxx.md` and update the README index.
- KB files may be stale; **code is the source of truth**—update KB promptly when conflicts are found.
- No need to commit KB files.

</WORKFLOW_INSTRUCTIONS>
