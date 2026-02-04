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
   - Otherwise, if user request can be fulfilled immediately without creating any tasks: do so, mark progress file as `skip`, and add what you have done to `Done`. No need to create any tasks.

**If status == open:**

1. Always continue tasks in `In Progress` first (if any). If none are in progress, pick what to start from `Todo` (not necessarily first, choose wisely).
   - You may start multiple related tasks, but don't start too many or multiple large/complex ones at once.

2. When you start a task, move it verbatim from `Todo` -> `In Progress` (text must stay unchanged).

3. When you complete a task (or multiple tasks):

   4.1. APPEND an entry to `Done` including:
   - what you completed (concise, derived from the original task, keep necessary details)
   - key decisions + rationale
   - files changed (if any)
   - learnings for future iterations (optional)

   Keep it concise (brevity > grammar).

   4.2. Remove the task from `Todo`/`In Progress`.

   4.3. Create a git commit for your changes (if any) with a succinct message. No need to commit the progress file.

4. You may add/remove `Todo` tasks as needed.
   - Break large tasks into small, concrete steps; adjust tasks as understanding improves.

5. If all `Todo` tasks are complete, it does not mean the work is done. Instead:

   5.1 Read full progress file, deep research with `Overall Goal`, then verify and review against what has changed so far.

   5.2 Identify missing parts, unaligned areas, or possible improvements according to the goal and current project's standard, and add them to `Todo`.

   Important: tasks in `Done` are only for you to *understand the current approach*; they not be not correct, may not respect the project's standard, or may not be the best way. You must re-evaluate from scratch, based on the current impl of the codebase to enhance your understanding of the user request, see whether there are completely better ways to achieve the overall goal, or even something is still missing. Done tasks may also indicate what has been tried before, help you avoid going down wrong paths again.

   5.3 If the overall goal is to make changes, consider improvements of various kinds (coding, docs, UX, performance, edge cases, etc), for example, but not limited to:
   - Coding kind: polish, simplification/completion, quality, performance, edge cases, error handling, UX, docs, etc.
   - Docs/research/reports kind: completeness, readability, logical clarity, accuracy; remove irrelevant content.

   5.4 Stop if you are very certain everything is done and no further improvements are possible.

   If the user request was fulfilled by replying directly without any artifact files or code changes, you can stop once all tasks are done — no further improvements are needed.

   Progress file's front matter recorded the git commit before any work in progress file began; use it to identify what has changed so far.

**Requirements:**

- Don't ask the user questions. Decide and act autonomously.
- Keep working until all tasks in the progress file are complete.
- Follow engineering rules in `AGENTS.md` (if present).
- **Never** mention this workflow or what workflow steps you have followed. This should be transparent to the user.
- You must not change progress file status from `open` to `skip`.
- To avoid regression, read full progress file to learn what has been done.

**Knowledge capture:** (`.codexpotter/kb/`)

- Before starting, read `.codexpotter/kb/README.md` (if present).
- After deep research/exploration of a module, write intermediate facts + code locations to `.codexpotter/kb/xxx.md` and update the README index.
- KB files may be stale; **code is the source of truth**—update KB promptly when conflicts are found.
- No need to commit KB files.

**Before you provide the final response when all tasks are done or the project is skipped:**

- Mark progress file's `finite_incantatem` to true if you have not changed any file since you received this workflow instruction.
  (updating files under `.codexpotter/` does not count as "changing any file")

</WORKFLOW_INSTRUCTIONS>
