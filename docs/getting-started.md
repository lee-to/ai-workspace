[Back to README](../README.md) · [CLI Reference →](cli.md)

# Getting Started

## Prerequisites

- Rust toolchain (1.85+ for edition 2024)
- SQLite is bundled — no separate install needed

## Installation

### From source

```bash
git clone <repo-url>
cd ai-workspace
cargo install --path .
```

The binary `ai-workspace` is installed to `~/.cargo/bin/`.

### Verify

```bash
ai-workspace --version
ai-workspace --help
```

## Concepts

| Concept | Description |
|---------|-------------|
| **Project** | A directory registered with `ai-workspace init` |
| **Group** | A named collection of projects that share context |
| **Shared item** | A file, directory, or note made available to group members |
| **Scope** | Visibility level of a note: `project` (private) or `group` (shared) |
| **Label** | An optional human-readable tag on a shared item |

## Scopes & Visibility

Every shared item has a **scope** that determines who can see it.

### Notes: project vs group scope

| Scope | Flag | Visible in project | Visible in group |
|-------|------|--------------------|------------------|
| **project** | `--scope project` | Yes | No |
| **group** | `--scope group --group <name>` | No | Yes (all member projects) |

- **Project-scoped notes** are private to the project that created them. They do not appear in group context. Use these for internal reminders or project-specific details.
- **Group-scoped notes** are visible to every project in the group. Use these for cross-project conventions, shared credentials locations, deploy instructions, etc.

The default scope for `note` is `group`.

### Files & directories: implicit group visibility

Files and directories are always attached to the project that shared them, but they are **automatically visible** to all groups the project belongs to. There is no `--scope` flag for `share` — group visibility is implicit through group membership.

```
Project A ──┐
             ├── Group "backend" ──→ sees files from A and B
Project B ──┘
```

### Summary

| Item type | Owned by | Visible in project | Visible in group |
|-----------|----------|--------------------|------------------|
| File / Directory | Project | Yes | Yes (via membership) |
| Project note | Project | Yes | **No** |
| Group note | Group | No | Yes (all members) |

## Quick Start

### 1. Initialize projects

```bash
cd ~/project-a
ai-workspace init --group backend

cd ~/project-b
ai-workspace init --group backend
```

Both projects now belong to the `backend` group.

### 2. Share files

```bash
cd ~/project-a
ai-workspace share src/schema.rs --label "db schema"
ai-workspace share docs/ --label "project docs"
```

### 3. Add notes

```bash
# Project-scoped note (only visible in this project's context)
ai-workspace note "Uses PostgreSQL 16" --label "db-note" --scope project

# Group-scoped note (visible to all projects in the group)
ai-workspace note "Deploy to staging before merging" --label "deploy-note" --scope group --group backend
```

### 4. Edit notes

```bash
# Change note content (notes only)
ai-workspace edit "db-note" --content "Uses PostgreSQL 16 + pgvector"

# Promote a project note to group scope
ai-workspace edit "db-note" --scope group --group backend

# Change just the label
ai-workspace edit "deploy-note" --label "release-checklist"
```

### 5. Check status

```bash
ai-workspace status
```

### 6. Start MCP server

```bash
ai-workspace serve
```

The server reads JSON-RPC requests from stdin and writes responses to stdout.

## Data Storage

The database is stored at `~/.ai-workspace/workspace.db` (all platforms).

Override with the `AI_WORKSPACE_DB` environment variable:

```bash
AI_WORKSPACE_DB=/custom/path/workspace.db ai-workspace status
```

## Logging

Set `RUST_LOG` to control log output:

```bash
RUST_LOG=debug ai-workspace status
RUST_LOG=info ai-workspace serve
```

## Next Steps

- [CLI Reference](cli.md) — All commands and options
- [MCP Server](mcp-server.md) — Integrate with AI coding tools

## See Also

- [CLI Reference](cli.md) — Complete command documentation
- [MCP Server](mcp-server.md) — MCP tools and integration guide
