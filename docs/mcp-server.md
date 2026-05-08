[← CLI Reference](cli.md) · [Back to README](../README.md) · [Contributing →](contributing.md)

# MCP Server

The MCP server exposes shared workspace context to AI agents via JSON-RPC over stdio. Start it with:

```bash
ai-workspace serve
```

## Configuration

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

Get workspace metadata: all projects, their groups, and shared items (no file content).

**Parameters:** none

**Returns:** JSON with `projects` and `groups` arrays. Each project includes its shared items (id, kind, path, label, dependencies). Each dependency includes the source service slug, dependency kind, and recommended reaction. Each group includes its member projects and group notes (with preview).

### `workspace_read`

Read the content of a shared file, directory, or note. Supports two modes: by shared item ID or by project ID + relative path.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `item_id` | integer | — | The shared item ID (mutually exclusive with `project_id`+`rel_path`) |
| `project_id` | integer | — | Project ID to read from (use with `rel_path`) |
| `rel_path` | string | — | Relative path within the project (use with `project_id`) |

Provide **either** `item_id` **or** `project_id`+`rel_path`, not both. Passing both returns an `invalid_params` error.

**Behavior:**
- **File:** returns file content as text (max 10 MB)
- **Directory:** returns listing of filenames
- **Note:** returns note content (only via `item_id`)
- Path traversal protection: rejects paths outside the project directory

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
| `shared_item_id` | integer | Shared item row id (use with `workspace_read`) |
| `project_id` | integer | Project the file belongs to |
| `path` | string | File path relative to the project root |
| `snippet` | string | Context snippet with matched terms wrapped in `[...]` |
| `rank` | number | bm25 score (lower = better match) |

**Indexing behavior:**
- Only `.md` files are indexed; non-markdown files, files >1 MB, and non-UTF-8 content are skipped.
- Files are indexed automatically when shared. Files whose mtime has changed on disk are lazily refreshed before each search (bounded to 200 per call).
- If the database predates FTS (or the index looks empty), run `ai-workspace reindex` once to populate it.

**vs `workspace_search`:** `workspace_search` searches note content only with sanitized terms; `workspace_search_fulltext` searches `.md` file content and accepts full FTS5 query syntax.

### `workspace_service_graph`

Inspect directional service links for all projects, a specific group, or the group graph around one project.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `project` | string | no | Project id, slug, or registered path whose group graph should be returned |
| `project_id` | integer | no | Project ID whose group graph should be returned |
| `group_id` | integer | no | Group ID whose service graph should be returned |

Pass at most one selector. With no selector, the tool returns all service links.

**Returns:** JSON object with a `scope` object and a `links` array. Each link includes id, source/target project ids and slugs, kind, label, and timestamps.

### `workspace_events`

List workspace events or show a project's open event inbox.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `project` | string | no | Project id, slug, or registered path for inbox mode |
| `project_id` | integer | no | Project ID for inbox mode |
| `source` | string | no | Source service slug filter for list mode |
| `status` | string | no | Event status filter: `open` or `closed` |

Project inbox mode cannot be combined with `source` or `status`. With no project selector, the tool lists events and applies the optional filters.

**Returns:** JSON array of events with source snapshots, kind, title, body, severity, status, and timestamps.

### `workspace_event_details`

Get one event with affected services and affected artifacts.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `event_id` | integer | yes | Workspace event ID |

**Returns:** JSON object with `event`, `affected_services`, and `affected_artifacts`. Artifact entries include path snapshots and recommended reactions.

### `project_tree`

List the file tree of a project, respecting `.gitignore` rules.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `project_id` | integer | yes | The project ID |
| `subdir` | string | no | Subdirectory to list (relative to project root) |
| `max_depth` | integer | no | Maximum traversal depth (1 = immediate children only, default: unlimited) |

**Returns:** Indented text tree of files and directories. Directories are suffixed with `/`. Entries excluded by `.gitignore` are not shown.

**Example output:**
```
Cargo.toml
README.md
src/
  main.rs
  lib.rs
```

### `project_grep`

Search project files for a regex pattern, respecting `.gitignore` rules.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `project_id` | integer | yes | The project ID |
| `pattern` | string | yes | Regex pattern to search for |
| `glob` | string | no | Glob to filter files (e.g. `*.rs`) |

**Returns:** Matches grouped by file with line numbers. Returns up to 100 matches. Skips binary files and files larger than 1 MB.

**Example output:**
```
src/main.rs:
  3:    println!("hello");
src/utils.rs:
  2:    println!("hello {}", name);
```

Invalid regex patterns return an `invalid_params` error.

### `list_groups`

List all groups with their member projects.

**Parameters:** none

**Returns:** Array of groups, each with id, name, and projects (id, name, path).

### `list_projects`

List all projects with their groups.

**Parameters:** none

**Returns:** Array of projects, each with id, name, path, and groups (id, name).

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
