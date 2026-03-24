[← Getting Started](getting-started.md) · [Back to README](../README.md) · [MCP Server →](mcp-server.md)

# CLI Reference

All commands are run as `ai-workspace <command>`.

## Working Directory Rules

Commands that operate on the "current project" must be run inside an initialized project directory (or any of its subdirectories):
- `share`
- `note`
- `edit`
- `rm`
- `leave`
- `status`
- `export`

If no project matches the current directory, these commands fail with:
`No project found for current directory. Run ai-workspace init first.`

Commands that work from any directory (no project required):
- `list`
- `delete-group`
- `sync`
- `serve`

## Commands

### `init`

Initialize the current directory as a project.

```bash
ai-workspace init [--name <name>] [--group <group>]
```

| Option | Description |
|--------|-------------|
| `-n, --name` | Project name (defaults to directory name) |
| `-g, --group` | Group to join or create |

If the directory is already initialized, running `init` again with `--group` adds the project to that group.

If `.ai-workspace.json` exists in the current directory, `init` reads it and applies the config (groups, shares, notes) via sync. The `--name` flag overrides the name from the JSON file; `--group` is additive to the groups listed in the file.

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

The path must exist and be within the project directory. Directories are shared as `dir` kind, files as `file` kind.

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

### `list`

List all projects and groups in the workspace.

```bash
ai-workspace list [all|projects|groups]
```

| Argument | Default | Description |
|----------|---------|-------------|
| `[what]` | `all` | What to list: `all`, `projects`, or `groups` |

Does not require being inside a project directory. Shows project paths and group memberships.

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

Show project info, groups, shared items, and group notes.

```bash
ai-workspace status
```

### `export`

Export the current project's config to `.ai-workspace.json` in the project root. This is the only way to create the file — other commands (`share`, `rm`, `note`, `edit`, `leave`) update it only if it already exists.

```bash
ai-workspace export
```

The exported file includes the project name, groups, shared files/dirs, and project-scoped notes. Group notes are not exported.

Commit this file to your repo so teammates can run `ai-workspace init` and get the same context automatically.

### `sync`

Verify shared files/dirs still exist on disk and reconcile with `.ai-workspace.json` if present.

```bash
ai-workspace sync
```

Two-step process:
1. Remove stale file/dir entries whose paths no longer exist on disk
2. If the current directory is inside a project and `.ai-workspace.json` exists, sync the database to match the config (add missing groups/shares/notes, remove extras, update changed notes)

### `serve`

Start the MCP server on stdio.

```bash
ai-workspace serve
```

See [MCP Server](mcp-server.md) for details on the available tools.

## See Also

- [Getting Started](getting-started.md) — Installation and first steps
- [MCP Server](mcp-server.md) — MCP tools exposed by `serve`
