# Plan: Project Navigation & Auto-Share

Created: 2026-03-24
Mode: fast
Refined: 2026-03-24

## Settings

- Testing: yes
- Logging: verbose (DEBUG level)
- Docs: yes (mandatory checkpoint)

## Research Context

Topic: Навигация по проектам для LLM-агентов
Goal: Дать агенту возможность видеть структуру чужих проектов, читать произвольные файлы, искать по содержимому — без ручного расшаривания каждого файла.
Decisions:
- Tree-sitter отложен (высокая сложность, средняя польза)
- Вместо индексации — навигация: дерево файлов + grep + чтение по пути
- Auto-share ключевых файлов при init для "паспорта проекта"

## Tasks

### Phase 1: Foundation

- [x] **Task 1: Add `ignore` and `regex` crate dependencies**
  - Files: `Cargo.toml`
  - `ignore = "0.4"` — walkdir + .gitignore
  - `regex = "1"` — regex for grep

- [x] **Task 2: Create `src/walk.rs` — file tree walker with .gitignore**
  - Blocked by: Task 1
  - Files: `src/walk.rs` (new), `src/models.rs`, `src/main.rs`
  - Add `mod walk;` to `src/main.rs`
  - Define structs (in `models.rs` or `walk.rs`):
    - `FileEntry { path: String, name: String, is_dir: bool }` with `Serialize`
    - `GrepMatch { path: String, line_number: usize, line_content: String }` with `Serialize`
  - `walk_project_tree(root, subpath?) -> Vec<FileEntry>` — uses `ignore::WalkBuilder`, respects .gitignore
  - `grep_project(root, pattern, glob?) -> Vec<GrepMatch>` — uses `regex` crate
  - Limits: 1MB per file for grep, 100 max matches, skip binary files
  - Binary detection: check for null bytes in first 8KB

### Phase 2: MCP Tools

- [x] **Task 3: Implement `project_tree` MCP tool**
  - Blocked by: Task 2
  - Files: `src/mcp/tools.rs`, `src/mcp/mod.rs`
  - Add `"project_tree"` branch to `handle_tool_call()` match
  - Params: `project_id` (required i64), `path` (optional subdirectory)
  - Resolve project via `db.get_project_by_id(project_id)` → `project.path` as root
  - Output: indented tree-like text (files/dirs with depth-based indentation)
  - Security: canonicalize + starts_with validation for path param (reuse existing pattern)
  - Update `handle_tools_list()` in `src/mcp/mod.rs` with inputSchema

- [x] **Task 4: Extend `workspace_read` to accept project_id + path**
  - **No blockers** — independently implementable
  - Files: `src/mcp/tools.rs`, `src/mcp/mod.rs`
  - Add `project_id` (i64) + `path` (String) as alternative params to existing `item_id`
  - Parameter priority: if both `item_id` AND `project_id+path` provided → return `invalid_params` error
  - Resolve project via `db.get_project_by_id(project_id)` → `project.path` as root
  - Reuse existing canonicalization + starts_with security pattern from current `workspace_read`
  - Same 10MB file size limit
  - Backward compatible: `item_id` still works
  - Update tool inputSchema in `handle_tools_list()` in `src/mcp/mod.rs`

- [x] **Task 5: Implement `project_grep` MCP tool**
  - Blocked by: Task 2
  - Files: `src/mcp/tools.rs`, `src/mcp/mod.rs`
  - Add `"project_grep"` branch to `handle_tool_call()` match
  - Params: `project_id` (required i64), `pattern` (required string), `glob` (optional string)
  - Resolve project via `db.get_project_by_id(project_id)` → `project.path` as root
  - Output: matches grouped by file with line numbers
  - Validate regex with `regex::Regex::new()`, return friendly `invalid_params` error on invalid pattern
  - Update `handle_tools_list()` in `src/mcp/mod.rs` with inputSchema

### Phase 3: Auto-Share

- [x] **Task 6: Implement auto-share key files on init**
  - **No blockers** — independently implementable
  - Files: `src/cli/mod.rs`
  - Insert after project create/update in init flow, ONLY when NO `.ai-workspace.json` exists
  - Detect & share via `db.share_file()`: README*, Cargo.toml, package.json, go.mod, pyproject.toml, composer.json, Makefile, Taskfile.yml, Justfile
  - Skip files already shared (no duplicates on re-init)
  - Log each auto-shared file at info level
  - Print count of auto-shared files in CLI output

### Phase 4: Tests

- [x] **Task 7: Integration tests for new MCP tools**
  - Blocked by: Tasks 3, 4, 5
  - Files: `tests/mcp_tests.rs`
  - project_tree: basic tree, subpath, gitignore respected, invalid project_id
  - workspace_read by project_id+path: basic read, path traversal attack, missing file, backward compat with item_id, error when both item_id and project_id+path provided
  - project_grep: basic match, glob filter, invalid regex error, no matches case
  - Follow existing test patterns: spawn binary in serve mode, seed via CLI, assert JSON-RPC responses

- [x] **Task 8: Integration tests for auto-share on init**
  - Blocked by: Task 6
  - Files: `tests/cli_tests.rs`
  - Auto-share Rust project (Cargo.toml + README.md detected)
  - Auto-share Node project (package.json detected)
  - Skip auto-share when .ai-workspace.json exists
  - No duplicates on re-init
  - Follow existing test patterns: tempfile isolation, AI_WORKSPACE_DB env var, run_cmd_in_dir helper

### Phase 5: Documentation

- [x] **Task 9: Documentation checkpoint**
  - Blocked by: Tasks 7, 8
  - Update: docs/mcp-server.md, docs/cli.md, docs/getting-started.md, README.md
  - Document new tools: project_tree, project_grep, workspace_read by path
  - Document auto-share behavior on init
  - Run /aif-docs

## Commit Plan

**Commit 1** (after Tasks 1-2): `feat: add walk module with file tree and grep support`
**Commit 2** (after Tasks 3-5): `feat: add project_tree, project_grep MCP tools and workspace_read by path`
**Commit 3** (after Task 6): `feat: auto-share key project files on init`
**Commit 4** (after Tasks 7-8): `test: integration tests for navigation tools and auto-share`
**Commit 5** (after Task 9): `docs: document project navigation and auto-share features`

## Dependency Graph

```
[1] Dependencies
 ├──► [2] walk.rs ──► [3] project_tree ──┐
 │                 ──► [5] project_grep ──┤
 │                                        ├──► [7] MCP tests ──┐
 [4] workspace_read by path (no blocker)──┤                     │
                                          └─────────────────────┤
[6] Auto-share (no blocker) ──► [8] CLI tests ──────────────────┴──► [9] Docs
```

## Refinement Notes (2026-03-24)

Changes applied:
- Task 2: Added explicit acceptance criteria — `mod walk;` declaration, `FileEntry`/`GrepMatch` struct definitions with serde, binary detection strategy
- Task 4: **Removed dependency on Task 1** (doesn't use ignore/regex). Added parameter priority rule, explicit DB resolution path
- Tasks 3, 4, 5: Added `src/mcp/mod.rs` to files list — must update `handle_tools_list()` with inputSchema
- Task 6: Added mechanism details — `db.share_file()` calls, insertion point in init flow, info-level logging
- Task 7: Added test case for dual-param error (item_id + project_id+path)
- Dependency graph updated: Task 4 and Task 6 are independently implementable (parallel track)
