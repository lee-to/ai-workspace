[← Getting Started](getting-started.md) · [Back to README](../README.md) · [MCP Server →](mcp-server.md)

# CLI Reference

All commands are run as `ai-workspace <command>`.

## Working Directory Rules

Commands that operate on the "current project" must be run inside an initialized project directory (or any of its subdirectories):
- `share`
- `artifact`
- `note`
- `edit`
- `rm`
- `leave`
- `destroy` without a target
- `status`
- `export`

If no project matches the current directory, these commands fail with:
`No project found for current directory. Run ai-workspace init first.`

Commands that work from any directory (no project required):
- `destroy <target>` or `destroy --target <target>`
- `list`
- `link add`, `link list --project`, `link rm`
- `delete-group`
- `sync`
- `search`
- `reindex`
- `serve`
- `update`

## Commands

### `init`

Initialize the current directory as a project.

```bash
ai-workspace init [--name <name>] [--slug <slug>] [--group <group>]
```

| Option | Description |
|--------|-------------|
| `-n, --name` | Project name (defaults to directory name) |
| `--slug` | Stable service slug (defaults to a normalized project name) |
| `-g, --group` | Group to join or create |

If the directory is already initialized, running `init` again is safe — the existing name is preserved unless `--name` is explicitly provided. Adding `--group` joins the project to that group.

If `.ai-workspace.json` exists in the current directory, `init` reads it and applies the config (groups, shares, notes) via sync. The `--name` flag overrides the name from the JSON file; `--group` is additive to the groups listed in the file. Shared paths from the config must exist, be relative paths, and resolve inside the project directory; path traversal and symlink escapes are rejected.

**Auto-share:** When no `.ai-workspace.json` exists, `init` automatically detects and shares key project files: `README*`, `Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, `composer.json`, `Makefile`, `Taskfile.yml`, `Justfile`. Already-shared files are skipped, so re-running `init` does not create duplicates.

### `share`

Share a file or directory with all project groups.

```bash
ai-workspace share <path> [--label <label>]
```

| Option | Description |
|--------|-------------|
| `<path>` | Relative path from project root |
| `-l, --label` | Human-readable label for the shared item |

The path must exist, be relative, and resolve within the project directory. Directories are shared as `dir` kind, files as `file` kind.

If `.ai-workspace.json` exists, it is automatically updated after a successful share.

### `note`

Add a text note scoped to the project or a group.

```bash
ai-workspace note <content> [--label <label>] [--scope <scope>] [--group <group>]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<content>` | — | Note text |
| `-l, --label` | — | Human-readable label |
| `-s, --scope` | `group` | `project` or `group` |
| `--group` | — | Group name (required when scope is `group`) |

### `edit`

Edit a shared item's content, label, or scope.

```bash
ai-workspace edit <target> [--content <text>] [--label <label>] [--scope <scope>] [--group <group>]
```

| Option | Description |
|--------|-------------|
| `<target>` | Item ID, label, or path to edit |
| `-c, --content` | New content (notes only) |
| `-l, --label` | New label (empty string clears it) |
| `-s, --scope` | Change scope to `project` or `group` |
| `--group` | Group name (required when scope is `group`) |

Target resolution follows the same order as `rm`: numeric ID, then label, then path.

At least one of `--content`, `--label`, or `--scope` must be provided.

Changing `--content` or `--scope` is only valid for notes. Files and directories only support `--label` changes.

### `leave`

Remove the current project from a group.

```bash
ai-workspace leave <group>
```

| Option | Description |
|--------|-------------|
| `<group>` | Group name to leave |

The group itself is not deleted — only the association with the current project is removed.

### `delete-group`

Delete a group entirely, including all associations and group-scoped shared items.

```bash
ai-workspace delete-group <group>
```

| Option | Description |
|--------|-------------|
| `<group>` | Group name to delete |

This removes all project associations with the group, all group-scoped notes, and the group itself. Does not require being inside a project directory.

### `destroy`

Remove a project from ai-workspace entirely.

```bash
ai-workspace destroy
ai-workspace destroy <target>
ai-workspace destroy --target <target>
```

| Option | Description |
|--------|-------------|
| `<target>` | Project ID, slug, or exact registered path |
| `--target <target>` | Project ID, slug, or exact registered path |

When no target is provided, `destroy` removes the current project and must be run inside an initialized project directory. When a target is provided, it can be run from any directory. This is useful for removing orphaned project records whose directories were renamed or deleted.

Before deleting the project record, `destroy` creates a `service_deleted` event and snapshots affected linked services/artifacts. Event creation, impact snapshotting, and deletion happen in one database transaction. The event history remains available after the project is gone.

Deletes the project record, all its shared items (files, directories, project-scoped notes), group memberships, and group-scoped notes created by this project. **Files on disk are not affected.**

After `destroy`, you can re-register the project with `ai-workspace init`.

### `list`

List all projects and groups in the workspace.

```bash
ai-workspace list [all|projects|groups]
```

| Argument | Default | Description |
|----------|---------|-------------|
| `[what]` | `all` | What to list: `all`, `projects`, or `groups` |

Does not require being inside a project directory. Shows project paths and group memberships.

### `link`

Manage directional service links between projects. Project endpoints can be a numeric id, slug, or registered path.

```bash
ai-workspace link add <from> <to> --kind <kind> [--label <label>]
ai-workspace link list [--project <project>]
ai-workspace link rm <id>
```

| Command | Description |
|---------|-------------|
| `link add` | Create or reuse a directional service link |
| `link list` | Inside a project, show the current group service graph; outside a project, show all links |
| `link list --project` | Show incoming and outgoing links for one project |
| `link rm` | Remove a service link by id |

Accepted link kinds:
- `depends_on`
- `related_to`

Example:

```bash
ai-workspace link add billing-api auth-service --kind depends_on --label "JWT validation"
ai-workspace link list --project billing-api
```

### `artifact`

Mark shared files or directories as depending on a service. Commands must run inside the owning project because item resolution is scoped to the current project.

```bash
ai-workspace artifact depends <item> <service-slug> --kind <kind> --reaction <reaction>
ai-workspace artifact deps [<item>]
ai-workspace artifact undepend <item> <service-slug> [--kind <kind>]
```

| Command | Description |
|---------|-------------|
| `artifact depends` | Add or update dependency metadata for a shared file/directory |
| `artifact deps` | List dependencies for all current-project artifacts or one item |
| `artifact undepend` | Remove dependency metadata for one item/service pair |

Accepted dependency kinds:
- `references`
- `consumes_api`
- `documents`
- `configures`

Accepted reactions:
- `inspect`
- `update`
- `delete`
- `remove_reference`

Examples:

```bash
ai-workspace artifact depends specs/auth.md auth-service --kind references --reaction update
ai-workspace artifact deps specs/auth.md
ai-workspace artifact undepend specs/auth.md auth-service --kind references
```

### `event`

Create, inspect, close, and remove workspace events. Events snapshot the source service slug/name so history remains readable after a project is destroyed.

```bash
ai-workspace event create --kind <kind> --source <slug> [--severity <level>] [--title <title>] [--body <text>]
ai-workspace event inbox
ai-workspace event list [--source <slug>] [--status <status>]
ai-workspace event show <id>
ai-workspace event close <id>
ai-workspace event rm <id>
```

Accepted event kinds:
- `service_deleted`
- `service_changed`
- `artifact_changed`

Accepted severities:
- `info`
- `warning`
- `error`
- `critical`

Accepted status filters:
- `open`
- `closed`

`event inbox` must run inside a project and shows open events affecting that project.

### `rm`

Remove a shared item by ID, label, or path.

```bash
ai-workspace rm <target>
```

Resolution order:
1. Try as numeric ID (scoped to current project)
2. Try as label match
3. Try as path match

### `status`

Show project info, groups, shared items, artifact dependency summaries, and group notes.

```bash
ai-workspace status
```

### `export`

Export the current project's config to `.ai-workspace.json` in the project root. This is the only way to create the file — other commands (`share`, `rm`, `note`, `edit`, `leave`) update it only if it already exists.

```bash
ai-workspace export
```

The exported file includes the project name, stable slug, groups, shared files/dirs, project-scoped notes, and artifact dependency metadata for shared files/directories. Shared entries are exported in object form with `path`, `kind`, optional `label`, and `dependencies` so directories sync back as directories. Group notes and workspace event history are not exported.

Older configs remain valid: string share entries such as `"README.md"` and object entries such as `{ "path": "README.md", "label": "Readme" }` still load. To sync artifact dependencies declaratively, add a `dependencies` array to the share object:

```json
{
  "path": "docs/auth.md",
  "kind": "file",
  "dependencies": [
    {
      "service": "auth",
      "kind": "references",
      "reaction": "update"
    }
  ]
}
```

Commit this file to your repo so teammates can run `ai-workspace init` and get the same context automatically.

### `sync`

Verify shared files/dirs still exist on disk and reconcile with `.ai-workspace.json` if present.

```bash
ai-workspace sync
```

Two-step process:
1. Remove stale file/dir entries whose paths no longer exist on disk
2. If the current directory is inside a project and `.ai-workspace.json` exists, sync the database to match the config (add missing groups/shares/notes, remove extras, update changed notes)

### `search`

Full-text search over shared `.md` files. Uses SQLite FTS5 with the `unicode61 remove_diacritics 2` tokenizer, ranked by bm25.

```bash
ai-workspace search <query> [--limit <n>]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<query>` | — | FTS5 query (supports phrase `"..."` and operators `AND` / `OR` / `NOT`) |
| `-l, --limit` | `20` | Maximum number of results |

**Output:** Each hit shows path, shared item id, bm25 rank (lower = better), and a snippet with matched terms wrapped in `[...]`.

**Query examples:**
- `deploy` — files containing "deploy"
- `"release checklist"` — exact phrase
- `deploy AND staging` — both terms
- `deploy NOT legacy` — "deploy" but not "legacy"

**Index coverage:**
- Only `.md` files are indexed.
- Files larger than 1 MB or with invalid UTF-8 are skipped.
- Indexing happens automatically on `share` and when `init` auto-shares files.
- Before each search, files whose mtime has changed on disk are lazily refreshed (bounded to 200 per call).
- Russian/English text is supported at the normalization level, but there is no stemming.

For existing databases created before FTS was added, run `reindex` once to populate the index.

### `reindex`

Rebuild the full-text index for all shared `.md` files across every project.

```bash
ai-workspace reindex
```

Walks every shared file and every `.md` inside every shared directory, re-reads content from disk, and rewrites the FTS index. Prints counts for indexed, skipped (size), skipped (non-UTF-8), and missing files. Does not require being inside a project directory.

### `serve`

Start the MCP server on stdio.

```bash
ai-workspace serve
```

See [MCP Server](mcp-server.md) for details on the available tools.

### `update`

Update ai-workspace to the latest version.

```bash
ai-workspace update
```

Checks the latest release on GitHub, downloads the appropriate binary for your platform, and replaces the current binary in place. No Rust or Cargo required.

If you're already on the latest version, it prints a message and exits without changes.

## See Also

- [Getting Started](getting-started.md) — Installation and first steps
- [MCP Server](mcp-server.md) — MCP tools exposed by `serve`
