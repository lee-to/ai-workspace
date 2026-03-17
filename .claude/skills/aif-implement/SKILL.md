---
name: aif-implement
description: Execute implementation tasks from the current plan. Works through tasks sequentially, marks completion, and preserves progress for continuation across sessions. Use when user says "implement", "start coding", "execute plan", or "continue implementation".
argument-hint: '[--list] [@plan-file] [task-id or "status"]'
allowed-tools: Read Write Edit Glob Grep Bash TaskList TaskGet TaskUpdate AskUserQuestion Questions
disable-model-invocation: false
---

# Implement - Execute Task Plan

Execute tasks from the plan, track progress, and enable session continuation.

## Workflow

### Step 0: Check Current State

**FIRST:** Determine what state we're in:

```
1. Parse arguments:
   - --list → list available plans only (no implementation; STOP)
   - @<path> → explicit plan file override (highest priority)
   - <number> → start from specific task
   - status → status-only mode
2. Check for uncommitted changes (git status)
3. Check current branch
```

### Step 0.list: List Available Plans (`--list`)

If `$ARGUMENTS` contains `--list`, run read-only plan discovery and stop.

```
1. Get current branch:
   git branch --show-current
2. Convert branch to filename: replace "/" with "-", add ".md"
3. Check existence of:
   - .ai-factory/plans/<branch-name>.md
   - .ai-factory/PLAN.md
   - .ai-factory/FIX_PLAN.md
4. Print plan availability summary and usage hints
5. STOP.
```

**Important:** In `--list` mode:
- Do not execute tasks
- Do not modify files
- Do not update TaskList statuses

For detailed output format and examples, see:
- `skills/aif-implement/references/IMPLEMENTATION-GUIDE.md` → "List Available Plans (`--list`)"

### Step 0.0: Resume / Recovery (after a break or after /clear)

If the user is resuming **the next day**, says the session was **abandoned**, or you suspect context was lost (e.g. after `/clear`), rebuild local context from the repo **before** continuing tasks:

```
1. git status
2. git branch --show-current
3. git log --oneline --decorate -20
4. (optional) git diff --stat
5. (optional) git stash list
```

Then reconcile plan/task state:
- Ensure the current plan file matches the current branch (`@plan-file` override wins; otherwise branch-named plan takes priority over `PLAN.md`).
- Compare `TaskList` statuses vs plan checkboxes.
  - If code changes for a task appear already implemented but the task is not marked completed, verify quickly and then `TaskUpdate(..., status: "completed")` and update the plan checkbox.
  - If a task is marked completed but the corresponding code is missing (rebase/reset happened), mark it back to pending and discuss with the user.

**If uncommitted changes exist:**
```
You have uncommitted changes. Commit them first?
- [ ] Yes, commit now (/aif-commit)
- [ ] No, stash and continue
- [ ] Cancel
```

**If NO plan file exists but `.ai-factory/FIX_PLAN.md` exists:**

A fix plan was created by `/aif-fix` in plan mode. Redirect to fix workflow:

```
Found a fix plan (.ai-factory/FIX_PLAN.md).

This plan was created by /aif-fix and should be executed through the fix workflow
(it creates a patch and handles cleanup automatically).

Running /aif-fix to execute the plan...
```

→ **Invoke `/aif-fix`** (without arguments — it will detect FIX_PLAN.md and execute it).
→ **STOP** — do not continue with implement workflow.

**If NO plan file exists AND no FIX_PLAN.md (all tasks completed or fresh start):**

```
No active plan found.

Current branch: feature/user-auth

What would you like to do?
- [ ] Start new feature from current branch
- [ ] Return to main/master and start new feature
- [ ] Create quick task plan (no branch)
- [ ] Nothing, just checking status
```

Based on choice:
- New feature from current → `/aif-plan full <description>`
- Return to main → `git checkout main`, then `git pull` → `/aif-plan full <description>`
- Quick task → `/aif-plan fast <description>`

**If plan file exists → continue to Step 0.1**

### Step 0.1: Load Project Context & Past Experience

**Read `.ai-factory/DESCRIPTION.md`** if it exists to understand:
- Tech stack (language, framework, database, ORM)
- Project architecture and conventions
- Non-functional requirements

**Read `.ai-factory/ARCHITECTURE.md`** if it exists to understand:
- Chosen architecture pattern and folder structure
- Dependency rules (what depends on what)
- Layer/module boundaries and communication patterns
- Follow these conventions when implementing — file placement, imports, module boundaries

**Read `.ai-factory/RULES.md`** if it exists:
- These are project-specific rules and conventions added by the user
- **ALWAYS follow these rules** when implementing — they override general patterns
- Rules are short, actionable — treat each as a hard requirement

**Read `.ai-factory/skill-context/aif-implement/SKILL.md`** — MANDATORY if the file exists.

This file contains project-specific rules accumulated by `/aif-evolve` from patches,
codebase conventions, and tech-stack analysis. These rules are tailored to the current project.

**How to apply skill-context rules:**
- Treat them as **project-level overrides** for this skill's general instructions
- When a skill-context rule conflicts with a general rule written in this SKILL.md,
  **the skill-context rule wins** (more specific context takes priority — same principle as nested CLAUDE.md files)
- When there is no conflict, apply both: general rules from SKILL.md + project rules from skill-context
- Do NOT ignore skill-context rules even if they seem to contradict this skill's defaults —
  they exist because the project's experience proved the default insufficient
- **CRITICAL:** skill-context rules apply to ALL outputs of this skill — including the code
  you write and how you update plan checkboxes. If a skill-context rule says "code MUST follow X"
  or "implementation MUST include Y" — you MUST comply. Writing code that violates skill-context
  rules is a bug.

**Enforcement:** After generating any output artifact, verify it against all skill-context rules.
If any rule is violated — fix the output before presenting it to the user.

**Patch fallback (limited, only when skill-context is missing):**

- If `.ai-factory/skill-context/aif-implement/SKILL.md` does not exist and `.ai-factory/patches/` exists:
  - Use `Glob` to find `*.md` files in `.ai-factory/patches/`
  - Sort patch filenames ascending (lexical), then select the last **10** (or fewer if less exist)
  - Read those selected patch files only
  - Prioritize **Root Cause** and **Prevention** sections
- If skill-context exists, do **not** read all patches by default.
  - Optionally read a few targeted recent patches only when a task clearly matches a known failure pattern.

**Use this context when implementing:**
- Follow the specified tech stack
- Use correct import patterns and conventions
- Apply proper error handling and logging as specified
- Avoid pitfalls documented in skill-context rules and relevant fallback patches

### Step 0.1: Find Plan File

**If `$ARGUMENTS` contains `@<path>`:**

Use this explicit plan file and skip automatic plan discovery.

```
1. Extract path after "@"
2. Resolve relative to project root (absolute paths are also valid)
3. If file does not exist:
   "Plan file not found: <path>
    Provide an existing markdown plan file, for example:
    - /aif-implement @.ai-factory/PLAN.md
    - /aif-implement @.ai-factory/plans/feature-user-auth.md"
   → STOP
4. If file is .ai-factory/FIX_PLAN.md:
   → invoke /aif-fix (ownership + cleanup workflow) and STOP
5. Otherwise use this file as the active plan
```

Then continue with normal execution using the selected plan file.

**If no `@<path>` override is provided, check plan files in this order:**

**Check for plan files in this order:**

```
1. Check current git branch:
   git branch --show-current
   → Convert branch name to filename: replace "/" with "-", add ".md"
   → Look for .ai-factory/plans/<branch-name>.md (e.g., feature/user-auth → .ai-factory/plans/feature-user-auth.md)
2. No branch-based plan → Check .ai-factory/PLAN.md
3. No branch-based plan and no .ai-factory/PLAN.md → Check .ai-factory/FIX_PLAN.md
   → If exists: invoke /aif-fix (handles its own workflow with patches) and STOP
```

**Priority:**
1. `@<path>` argument - explicit user-selected plan file
2. Branch-named file (from `/aif-plan full`) - if it matches current branch
3. `.ai-factory/PLAN.md` (from `/aif-plan fast`) - fallback when no branch-based plan exists
4. `.ai-factory/FIX_PLAN.md` - redirect to `/aif-fix` (from `/aif-fix` plan mode)

**Read the plan file** to understand:
- Context and settings (testing, logging preferences)
- Commit checkpoints (when to commit)
- Task dependencies
- Task checklist format (`- [ ]` / `- [x]`) to keep progress synced

### Step 1: Load Current State

```
TaskList → Get all tasks with status
```

Find:
- Next pending task (not blocked, not completed)
- Any in_progress tasks (resume these first)

### Step 2: Display Progress

```
## Implementation Progress

✅ Completed: 3/8 tasks
🔄 In Progress: Task #4 - Implement search service
⏳ Pending: 4 tasks

Current task: #4 - Implement search service
```

### Step 3: Execute Current Task

For each task:

**3.1: Fetch full details**
```
TaskGet(taskId) → Get description, files, context
```

**3.2: Mark as in_progress**
```
TaskUpdate(taskId, status: "in_progress")
```

**3.3: Implement the task**
- Read relevant files
- Make necessary changes
- Follow existing code patterns
- **NO tests unless plan includes test tasks**
- **NO reports or summaries**

**3.4: Verify implementation**
- Check code compiles/runs
- Verify functionality works
- Fix any immediate issues

**3.5: Mark as completed**
```
TaskUpdate(taskId, status: "completed")
```

**3.6: Update checkbox in plan file**

**IMMEDIATELY** after completing a task, update the checkbox in the plan file:

```markdown
# Before
- [ ] Task 1: Create user model

# After
- [x] Task 1: Create user model
```

**This is MANDATORY** — checkboxes must reflect actual progress:
- Use `Edit` tool to change `- [ ]` to `- [x]`
- Do this RIGHT AFTER each task completion
- Even if deletion will be offered later
- Plan file is the source of truth for progress

**3.7: Update .ai-factory/DESCRIPTION.md if needed**

If during implementation:
- New dependency/library was added
- Tech stack changed (e.g., added Redis, switched ORM)
- New integration added (e.g., Stripe, SendGrid)
- Architecture decision was made

→ Update `.ai-factory/DESCRIPTION.md` to reflect the change:

```markdown
## Tech Stack
- **Cache:** Redis (added for session storage)
```

This keeps .ai-factory/DESCRIPTION.md as the source of truth.

**3.7.1: Update AGENTS.md and ARCHITECTURE.md if project structure changed**

If during implementation:
- New directories or modules were created
- Project structure changed significantly (new `src/modules/`, new API routes directory, etc.)
- New entry points or key files were added

→ Update `AGENTS.md` — refresh the "Project Structure" tree and "Key Entry Points" table to reflect new directories/files.

→ Update `.ai-factory/ARCHITECTURE.md` — if new modules or layers were added that should be documented in the folder structure section.

**Only update if structure actually changed** — don't rewrite on every task. Check if new directories were created that aren't in the current structure map.

**3.8: Check for commit checkpoint**

If the plan has commit checkpoints and current task is at a checkpoint:
```
✅ Tasks 1-4 completed.

This is a commit checkpoint. Ready to commit?
Suggested message: "feat: add base models and types"

- [ ] Yes, commit now (/aif-commit)
- [ ] No, continue to next task
- [ ] Skip all commit checkpoints
```

**3.9: Move to next task or pause**

### Step 4: Session Persistence

Progress is automatically saved via TaskUpdate.

**To pause:**
```
Current progress saved.

Completed: 4/8 tasks
Next task: #5 - Add pagination support

To resume later, run:
/aif-implement
```

**To resume (next session):**
```
/aif-implement
```
→ Automatically finds next incomplete task

### Step 5: Completion

When all tasks are done:

```
## Implementation Complete

All 8 tasks completed.

Branch: feature/product-search
Plan file: .ai-factory/plans/feature-product-search.md
Files modified:
- src/services/search.ts (created)
- src/api/products/search.ts (created)
- src/types/search.ts (created)
Documentation: updated existing docs | created docs/<feature-slug>.md | skipped by user | warn-only (Docs: no/unset)

What's next?

1. 🔍 /aif-verify — Verify nothing was missed (recommended)
2. 💾 /aif-commit — Commit the changes directly
```

**Check ROADMAP.md progress:**

If `.ai-factory/ROADMAP.md` exists:
1. Read it
1.1. If the plan file includes `## Roadmap Linkage` with a non-`none` milestone, prefer that milestone for completion marking
2. Check if the completed work corresponds to any unchecked milestone
3. If yes — mark it `[x]` and add entry to the Completed table with today's date
4. Tell the user which milestone was marked done

### Context Maintenance (Artifacts)

Only do this step when there is something concrete to capture.

**DESCRIPTION.md (allowed in this command):**
- If this plan introduced new dependencies/integrations or changed the stack, update `.ai-factory/DESCRIPTION.md` with factual deltas only.
- Do not rewrite unrelated sections.

**ARCHITECTURE.md + AGENTS.md (allowed in this command):**
- If new modules/layers/folders were added (or dependency rules changed), update `.ai-factory/ARCHITECTURE.md` to reflect the new structure and constraints.
- If you maintain `AGENTS.md` structure maps or entry points, refresh them only when they are now incorrect.

**ROADMAP.md (allowed, limited):**
- This command may mark milestone completion when evidence is clear.
- If milestone mapping is ambiguous, emit `WARN [roadmap] ...` and suggest the owner command:
  - `/aif-roadmap check`
  - or `/aif-roadmap <short update request>`

**RULES.md (NOT allowed in this command):**
- Never edit `.ai-factory/RULES.md` from `/aif-implement`.
- If you discovered repeatable conventions/pitfalls during implementation, propose up to 3 candidate rules and ask the user to add them via `/aif-rules`.
- Do not invoke `/aif-rules` automatically (it is user-invoked).

If candidate rules exist:

```
AskUserQuestion: Capture new project rules in `.ai-factory/RULES.md`?

Options:
1. Yes — output `/aif-rules ...` commands (recommended)
2. No — skip
```

### Context Cleanup

Context is heavy after implementation. All code changes are saved — suggest freeing space:

```
AskUserQuestion: Free up context before continuing?

Options:
1. /clear — Full reset (recommended)
2. /compact — Compress history
3. Continue as is
```

**Suggest verification:**

```
AskUserQuestion: All tasks complete. Run verification?

Options:
1. Verify first — Run /aif-verify to check completeness (recommended)
2. Skip to commit — Go straight to /aif-commit
```

If user chooses "Verify first" → suggest invoking `/aif-verify`.
If user chooses "Skip to commit" → suggest invoking `/aif-commit`.

**Documentation policy checkpoint (after completion, before plan cleanup):**

Read the plan file setting `Docs: yes/no`.

If plan setting is `Docs: yes`:
```
AskUserQuestion: Documentation checkpoint — how should we document this feature?

Options:
1. Update existing docs (recommended) — invoke /aif-docs
2. Create a new feature doc page — invoke /aif-docs with feature-page context
3. Skip documentation
```

Handling:
- Option 1 → invoke `/aif-docs` to update README/docs based on completed work
- Option 2 → invoke `/aif-docs` with context to create `docs/<feature-slug>.md`, include sections (Summary, Usage/user-facing behavior, Configuration, API/CLI changes, Examples, Troubleshooting, See Also), and add a README docs-table link
- Option 3 → do not invoke `/aif-docs`; emit `WARN [docs] Documentation skipped by user`

If plan setting is `Docs: no` or setting is unset:
- Do **not** show a mandatory docs checkpoint prompt
- Do **not** invoke `/aif-docs` automatically
- Emit `WARN [docs] Docs policy is no/unset; skipping documentation checkpoint`

**Always include documentation outcome in the final completion output:**
- `Documentation: updated existing docs`
- `Documentation: created docs/<feature-slug>.md`
- `Documentation: skipped by user`
- `Documentation: warn-only (Docs: no/unset)`

**Handle plan file after completion:**

- **If `.ai-factory/PLAN.md`** (from `/aif-plan fast`):
  ```
  Would you like to delete .ai-factory/PLAN.md? (It's no longer needed)
  - [ ] Yes, delete it
  - [ ] No, keep it
  ```

- **If branch-named file** (e.g., `.ai-factory/plans/feature-user-auth.md`):
  - Keep it - documents what was done
  - User can delete before merging if desired

**Check if running in a git worktree:**

Detect worktree context:
```bash
# If .git is a file (not a directory), we're in a worktree
[ -f .git ]
```

**If we ARE in a worktree**, offer to merge back and clean up:

```
You're working in a parallel worktree.

  Branch:    <current-branch>
  Worktree:  <current-directory>
  Main repo: <main-repo-path>

Would you like to merge this branch into main and clean up?
- [ ] Yes, merge and clean up (recommended)
- [ ] No, I'll handle it manually
```

If user chooses **"Yes, merge and clean up"**:

1. **Ensure everything is committed** — check `git status`. If uncommitted changes exist, suggest `/aif-commit` first and wait.

2. **Get main repo path:**
   ```bash
   MAIN_REPO=$(git rev-parse --git-common-dir | sed 's|/\.git$||')
   BRANCH=$(git branch --show-current)
   ```

3. **Switch to main repo:**
   ```bash
   cd "${MAIN_REPO}"
   ```

4. **Merge the branch:**
   ```bash
   git checkout main
   git pull origin main
   git merge "${BRANCH}"
   ```

   If merge conflict occurs:
   ```
   ⚠️  Merge conflict detected. Resolve manually:
     cd <main-repo-path>
     git merge --abort   # to cancel
     # or resolve conflicts and git commit
   ```
   → STOP here, do not proceed with cleanup.

5. **Remove worktree and branch (only if merge succeeded):**
   ```bash
   git worktree remove <worktree-path>
   git branch -d "${BRANCH}"
   ```

6. **Confirm:**
   ```
   ✅ Merged and cleaned up!

     Branch <branch> merged into main.
     Worktree removed.

   You're now in: <main-repo-path> (main)
   ```

If user chooses **"No, I'll handle it manually"**, show a reminder:
```
To merge and clean up later:
  cd <main-repo-path>
  git merge <branch>
  /aif-plan --cleanup <branch>
```

**IMPORTANT: NO summary reports, NO analysis documents, NO wrap-up tasks.**

## Commands

### Start/Resume Implementation
```
/aif-implement
```
Continues from next incomplete task.

### List Available Plans
```
/aif-implement --list
```
Lists `.ai-factory/PLAN.md`, `.ai-factory/FIX_PLAN.md`, and current-branch `.ai-factory/plans/<branch>.md` (if present), then exits without implementation.

### Use Explicit Plan File
```
/aif-implement @my-custom-plan.md
/aif-implement @.ai-factory/plans/feature-user-auth.md status
```
Uses the provided plan file instead of auto-detecting by branch/default files.

### Start from Specific Task
```
/aif-implement 5
```
Starts from task #5 (useful for skipping or re-doing).

### Check Status Only
```
/aif-implement status
```
Shows progress without executing.

## Execution Rules

### DO:
- ✅ Execute one task at a time
- ✅ Mark tasks in_progress before starting
- ✅ Mark tasks completed after finishing
- ✅ Follow existing code conventions
- ✅ Follow `/aif-best-practices` guidelines (naming, structure, error handling)
- ✅ Create files mentioned in task description
- ✅ Handle edge cases mentioned in task
- ✅ Stop and ask if task is unclear

### DON'T:
- ❌ Write tests (unless explicitly in task list)
- ❌ Create report files
- ❌ Create summary documents
- ❌ Add tasks not in the plan
- ❌ Skip tasks without user permission
- ❌ Mark incomplete tasks as done
- ❌ Violate `.ai-factory/ARCHITECTURE.md` conventions for file placement and module boundaries

## Artifact Ownership Boundaries

- Primary ownership in this command: task execution state and plan progress checkboxes.
- Allowed context artifact updates: `.ai-factory/DESCRIPTION.md`, `.ai-factory/ARCHITECTURE.md`, and roadmap milestone completion in `.ai-factory/ROADMAP.md` when implementation evidence justifies it.
- Read-only context in this command by default: `.ai-factory/RULES.md`, `.ai-factory/RESEARCH.md`.
- Context-gate findings should be communicated as `WARN`/`ERROR` outputs only; this does not replace the required verbose implementation logging rules below.

For progress display format, blocker handling, session continuity examples, and full flow examples → see `references/IMPLEMENTATION-GUIDE.md`

## Critical Rules

1. **NEVER write tests** unless task list explicitly includes test tasks
2. **NEVER create reports** or summary documents after completion
3. **ALWAYS mark task in_progress** before starting work
4. **ALWAYS mark task completed** after finishing
5. **ALWAYS update checkbox in plan file** - `- [ ]` → `- [x]` immediately after task completion
6. **PRESERVE progress** - tasks survive session boundaries
7. **ONE task at a time** - focus on current task only

## CRITICAL: Logging Requirements

**ALWAYS add verbose logging when implementing code.** For logging guidelines, patterns, and management requirements → read `references/LOGGING-GUIDE.md`

Key rules: log function entry/exit, state changes, external calls, error context. Use structured logging, configurable log levels (LOG_LEVEL env var).

**DO NOT skip logging to "keep code clean" - verbose logging is REQUIRED during implementation, but MUST be configurable.**
