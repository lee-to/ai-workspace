[← CLI Reference](cli.md) · [Back to README](../README.md)

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

**Returns:** JSON with `projects` and `groups` arrays. Each project includes its shared items (id, kind, path, label). Each group includes its member projects and group notes (with preview).

### `workspace_read`

Read the content of a shared file, directory, or note. Supports two modes: by shared item ID or by project ID + relative path.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `item_id` | integer | — | The shared item ID (mutually exclusive with `project_id`+`path`) |
| `project_id` | integer | — | Project ID to read from (use with `path`) |
| `path` | string | — | Relative path within the project (use with `project_id`) |

Provide **either** `item_id` **or** `project_id`+`path`, not both. Passing both returns an `invalid_params` error.

**Behavior:**
- **File:** returns file content as text (max 10 MB)
- **Directory:** returns listing of filenames
- **Note:** returns note content (only via `item_id`)
- Path traversal protection: rejects paths outside the project directory

### `workspace_search`

Full-text search over shared notes.

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

### `project_tree`

List the file tree of a project, respecting `.gitignore` rules.

**Parameters:**

| Name | Type | Required | Description |
|------|------|----------|-------------|
| `project_id` | integer | yes | The project ID |
| `path` | string | no | Subdirectory to list (relative to project root) |

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
