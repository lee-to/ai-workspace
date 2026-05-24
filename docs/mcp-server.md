[← CLI Reference](cli.md) · [Back to README](../README.md) · [Contributing →](contributing.md)

# MCP Server

The MCP server exposes shared workspace context to AI agents via JSON-RPC over stdio. Start it with:

```bash
ai-workspace serve
```

## Configuration

### MCP server scope

By default, `ai-workspace serve` runs in global scope and MCP tools can return metadata/search results for every project and group in the local workspace database. Use server scope when the MCP process should only expose one project or group:

```bash
ai-workspace serve --scope current-project
ai-workspace serve --group backend
ai-workspace serve --project api
```

Supported scopes are `global`, `current-project`, `group`, and `project`. `--group <name>` implies group scope, and `--project <id|slug|path>` implies project scope. CLI flags override environment variables.

Environment equivalents:

```bash
AI_WORKSPACE_SCOPE=current-project
AI_WORKSPACE_SCOPE=group AI_WORKSPACE_SCOPE_GROUP=backend
AI_WORKSPACE_SCOPE=project AI_WORKSPACE_SCOPE_PROJECT=api
```

Scope applies to every MCP tool: context, note search, full-text file search, lists, service graphs, events, event details, reads, project tree, and project grep. Project/current-project scope is strict: it exposes only the selected project’s project-scoped items plus its group names and group notes. Group scope exposes only projects, items, notes, service links, and events visible through that group.

### Project-wide MCP tools opt-in

By default, MCP tools expose only explicit shared scopes: shared files, shared directories, and notes. Project-wide reads, tree walks, grep, and absolute project path metadata are disabled unless you opt in.

Set `AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1` on the MCP server process to restore full project access inside the configured MCP scope:

```json
{
  "mcpServers": {
    "ai-workspace": {
      "command": "ai-workspace",
      "args": ["serve"],
      "env": {
        "AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS": "1"
      }
    }
  }
}
```

### Claude Code (CLI)

```bash
# Global — available in all projects
claude mcp add --scope user ai-workspace -- ai-workspace serve

# Current project only
claude mcp add ai-workspace -- ai-workspace serve
```

Verify with `claude mcp list`.

### Claude Desktop / other MCP clients

Add to your MCP config JSON:

```json
{
  "mcpServers": {
    "ai-workspace": {
      "command": "ai-workspace",
      "args": ["serve"]
    }
  }
}
```

## Protocol

- **Transport:** stdio (line-delimited JSON)
- **Protocol version:** `2024-11-05`
- **Capabilities:** `tools`

## Tools

### `workspace_context`

Get workspace metadata: visible projects, their groups, and shared items (no file content).

**Parameters:** none

**Returns:** JSON with scoped `projects` and `groups` arrays. Each project includes its shared items (id, kind, path, label, dependencies). Each dependency includes the source service slug, dependency kind, and recommended reaction. Each group includes its visible member projects and group notes (with preview). Absolute project paths are omitted unless `AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1` is set.

### `workspace_read`

Read the content of a shared file, directory, or note. Supports two modes: by shared item ID or by project ID + relative path.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `item_id` | integer | — | The shared item ID (mutually exclusive with `project_id`+`rel_path`) |
| `project_id` | integer | — | Project ID to read from (use with `rel_path`) |
| `rel_path` | string | — | Relative path within the project (use with `project_id`) |
| `include_hidden` | boolean | no | Include hidden/dotfile paths (default: `false`) |
| `include_sensitive` | boolean | no | Include credential-like paths such as `.env`, `.ssh`, `.aws`, `*.pem`, and `*.key` (default: `false`) |

Provide **either** `item_id` **or** `project_id`+`rel_path`, not both. Passing both returns an `invalid_params` error.

**Behavior:**
- **File:** returns file content as text (max 10 MB)
- **Directory:** returns listing of filenames
- **Note:** returns note content (only via `item_id`)
- `item_id` reads continue to work for shared files and directories
- `project_id`+`rel_path` is limited to an explicitly shared file or a path inside an explicitly shared directory by default
- Set `AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1` to allow project-wide `rel_path` reads inside the configured MCP scope
- Path traversal protection: rejects absolute paths, parent-directory traversal, and paths outside the project directory
- Hidden/dotfile and credential-like paths are blocked by default. Hidden sensitive paths such as `.ssh/id_rsa` require both `include_hidden: true` and `include_sensitive: true`.
- Explicitly shared `.ai-factory` context files are a narrow exception: non-sensitive `.ai-factory/...` paths are readable/searchable by default for the AI Factory preset. This exception is path-scoped and does not make hidden files in other shared directories visible.

### `workspace_search`

Full-text search over shared **notes** (not files — use `workspace_search_fulltext` for `.md` file contents).

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `query` | string | yes | Search query text |

**Returns:** Array of matching notes with id, label, group_id, project_id, content, and created_at.

**Query behavior:**
- Uses SQLite FTS5 under the hood, but user input is sanitized into safe term matching.
- Special operators are treated as plain text. Boolean operators (`OR`, `NOT`) and phrase syntax are not supported.

**Query examples:**
- `deploy` — notes containing "deploy"
- `staging environment` — notes containing both terms
- `release checklist` — notes containing both terms

### `workspace_search_fulltext`

Full-text search over shared `.md` **files** (including `.md` files inside shared directories). Uses SQLite FTS5 with the `unicode61 remove_diacritics 2` tokenizer, ranked by bm25.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `query` | string | yes | FTS5 query (supports phrase `"..."` and operators `AND` / `OR` / `NOT`) |
| `limit` | integer | no | Maximum number of results (default: 20) |

**Returns:** JSON array of hits, each with:

| Field | Type | Description |
|-------|------|-------------|
| `shared_item_id` | integer | Owning shared item row id |
| `project_id` | integer | Project the file belongs to |
| `path` | string | File path relative to the project root |
| `snippet` | string | Context snippet with matched terms wrapped in `[...]` |
| `rank` | number | bm25 score (lower = better match) |

Use `workspace_read` with `project_id` and `rel_path` set to the returned `path` to read the exact matched file. This matters for hits inside shared directories because `shared_item_id` points to the shared directory item.

**Indexing behavior:**
- Only `.md` files are indexed; non-markdown files, files >1 MB, and non-UTF-8 content are skipped.
- Hidden/dotfile and credential-like `.md` paths are skipped or removed from the index, including direct file shares and files inside shared directories.
- Files are indexed automatically when shared. Each `.md` file inside a shared directory is indexed as its own search document, so hits point at the exact child path.
- Files whose mtime has changed on disk are lazily refreshed before each search with a bounded budget (200 indexed rows or not-yet-indexed shared file/dir items per call).
- Deleted indexed child files are removed during lazy refresh; newly added child files inside already-indexed shared directories are picked up by `ai-workspace reindex`.
- If the database predates FTS (or the index looks empty), run `ai-workspace reindex` once to populate it.

**vs `workspace_search`:** `workspace_search` searches note content only with sanitized terms; `workspace_search_fulltext` searches `.md` file content and accepts full FTS5 query syntax.

### `workspace_service_graph`

Inspect directional service links visible inside the configured MCP scope.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `project` | string | no | Project id, slug, or registered path whose group graphs should be returned |
| `project_id` | integer | no | Project ID whose group graphs should be returned |
| `group_id` | integer | no | Group ID whose service graph should be returned |

Pass at most one selector. With no selector, the tool returns all service links visible inside the configured MCP scope. Selectors outside the server scope return a tool-level access error.

**Returns:** JSON object with a `scope` object and a `links` array. For project-scoped requests, `scope.groups` lists every group included in the graph. Each link includes id, source/target project ids and slugs, kind, label, and timestamps.

### `workspace_events`

List workspace events visible inside the configured MCP scope or show an in-scope project's open event inbox.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `project` | string | no | Project id, slug, or registered path for inbox mode |
| `project_id` | integer | no | Project ID for inbox mode |
| `source` | string | no | Source service slug filter for list mode |
| `status` | string | no | Event status filter: `open` or `closed` |

Project inbox mode cannot be combined with `source` or `status`. With no project selector, the tool lists scoped events and applies the optional filters.

**Returns:** JSON array of events with source snapshots, legacy `group_id`, source group snapshot `group_ids`, kind, title, body, severity, status, and timestamps.

### `workspace_event_details`

Get one in-scope event with affected services and affected artifacts.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `event_id` | integer | yes | Workspace event ID |

**Returns:** JSON object with `event`, `affected_services`, and `affected_artifacts`. Affected services and artifacts outside the configured MCP scope are filtered out. Artifact entries include path snapshots and recommended reactions.

### `project_tree`

List the shared file tree of an in-scope project, respecting `.gitignore` rules. By default this only returns explicitly shared files, explicitly shared directories, and visible ancestors needed to display them. Set `AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1` to list the full project tree inside the configured MCP scope.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `project_id` | integer | yes | The project ID |
| `subdir` | string | no | Subdirectory to list. By default it must intersect a shared scope |
| `max_depth` | integer | no | Maximum traversal depth (1 = immediate children only, default: unlimited) |
| `include_hidden` | boolean | no | Include hidden/dotfile paths (default: `false`) |
| `include_sensitive` | boolean | no | Include credential-like paths such as `.env`, `.ssh`, `.aws`, `*.pem`, and `*.key` (default: `false`) |

**Returns:** Indented text tree of files and directories. Directories are suffixed with `/`. Entries excluded by `.gitignore` are not shown. Hidden/dotfile and credential-like paths are hidden by default; hidden sensitive paths require both opt-in flags. Non-sensitive explicitly shared `.ai-factory/...` context can appear by default, but that exception does not apply to hidden files under other shared scopes.

**Example output:**
```
src/
  lib.rs
docs/
  guide.md
```

### `project_grep`

Search shared files in an in-scope project for a regex pattern, respecting `.gitignore` rules. By default this scans only explicitly shared files and files inside explicitly shared directories. Set `AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1` to search the full project inside the configured MCP scope.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `project_id` | integer | yes | The project ID |
| `pattern` | string | yes | Regex pattern to search for |
| `glob` | string | no | Glob to filter files (e.g. `*.rs`) |
| `include_hidden` | boolean | no | Include hidden/dotfile paths (default: `false`) |
| `include_sensitive` | boolean | no | Include credential-like paths such as `.env`, `.ssh`, `.aws`, `*.pem`, and `*.key` (default: `false`) |

**Returns:** Matches grouped by file with line numbers. Returns up to 100 matches. Skips binary files, files larger than 1 MB, and hidden/dotfile or credential-like paths unless explicitly included. Unshared files are not opened in the default mode. Non-sensitive explicitly shared `.ai-factory/...` context is searchable by default without broadening hidden-file access for other shared scopes.

**Example output:**
```
src/main.rs:
  3:    println!("hello");
src/utils.rs:
  2:    println!("hello {}", name);
```

Invalid regex patterns return an `invalid_params` error.

### `list_groups`

List groups visible inside the configured MCP scope with their scoped member projects.

**Parameters:** none

**Returns:** Array of groups, each with id, name, and scoped projects (id, name). Project paths are omitted unless `AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1` is set.

### `list_projects`

List projects visible inside the configured MCP scope with their groups.

**Parameters:** none

**Returns:** Array of projects, each with id, name, and groups (id, name). Project paths are omitted unless `AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1` is set.

## Error Handling

| Code | Meaning |
|------|---------|
| `-32700` | Parse error (malformed JSON) |
| `-32601` | Method not found / unknown tool |
| `-32602` | Invalid params (missing required parameter) |

Tool-level errors return a successful JSON-RPC response with `isError: true` in the result content.

## See Also

- [CLI Reference](cli.md) — CLI commands for managing projects and shared items
- [Getting Started](getting-started.md) — Installation and setup
