<WORKFLOW_INSTRUCTIONS>

Run the workflow below to make steady progress toward the overall goal recorded in the progress file. Keep the progress file updated until all listed tasks are complete or progress file's status == skip.

- Progress file: `{{PROGRESS_FILE}}`
- `.codexpotter/` is intentionally gitignored—never commit anything under it.
- Sections in progress file: Overall Goal, In Progress, Todo, Done
- Progress file's status in front matter: initial / open / skip

**If status == initial:**

1. Resolve user's request in `Overall Goal`.

2. Summarize it into a short title (max 10 words) using the same language as user's request and set it to progress file's `short_title` in front matter.

3. Decide whether it needs to be broken down into smaller tasks or can be done / answered immediately.
   - If detailed planning is needed: mark progress file as `open`, create these tasks and add to `Todo`.
   - If user request can be completed immediately: do so, mark progress file as `skip`. No need to create any tasks.

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
   - Identify possible improvements and add them to `Todo`, if user was asking to make changes:
     - Coding kind: polish, simplification/completion, quality, performance, edge cases, error handling, UX, docs, etc.
     - Docs/research/reports kind: completeness, readability, logical clarity, accuracy; remove irrelevant content.
   - Stop if you are very certain everything is done and no further improvements are possible.

   Important: if the user request was fulfilled by replying directly without any artifact files or code changes, you can stop once all tasks are done — no further improvements are needed.

**Requirements:**

- Don't ask the user questions. Decide and act autonomously.
- Keep working until all tasks in the progress file are complete.
- Follow engineering rules in `AGENTS.md` (if present).
- No need to respond what workflow steps you have followed. Just do them.
- You must not change progress file status from `open` to `skip`.
- To avoid regression, read full progress file to learn what has been done.

**Knowledge capture:** (`.codexpotter/kb/`)

- Before starting, read `.codexpotter/kb/README.md` (if present).
- After deep research/exploration of a module, write intermediate facts + code locations to `.codexpotter/kb/xxx.md` and update the README index.
- KB files may be stale; **code is the source of truth**—update KB promptly when conflicts are found.
- No need to commit KB files.

**Before you provide the final response when all tasks are done or the project is skipped:**

- Mark progress file's `potterflag` to true if you have not changed any file since you received this workflow instruction.
  (updating files under `.codexpotter/` does not count as "changing any file")

</WORKFLOW_INSTRUCTIONS>
