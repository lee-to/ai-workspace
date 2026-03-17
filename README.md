# AI Workspace

**Give your AI agents memory that spans across projects.**

AI coding agents (Claude Code, Cursor, Windsurf, etc.) are powerful — but they only see one project at a time. When your work involves a shared API contract, a common deploy process, or a monorepo split into microservices, the agent starts every conversation blind to the bigger picture.

AI Workspace fixes that. It's a lightweight CLI + [MCP server](https://modelcontextprotocol.io) that lets you share files, directories, and notes across related projects. Your agents get cross-project context automatically — no copy-pasting, no symlinks, no custom prompts.

## How it works

```
┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│  project-api │     │ project-web  │     │ project-docs │
│              │     │              │     │              │
│  schema.rs ──┼─┐   │              │     │              │
│  notes ──────┼─┤   │  notes ──────┼─┐   │  guides/ ────┼─┐
└──────────────┘ │   └──────────────┘ │   └──────────────┘ │
                 │                    │                    │
                 └────────┬───────────┘────────────────────┘
                          │
                    ┌─────▼─────┐
                    │  "backend" │  ← group
                    │   group    │
                    └─────┬─────┘
                          │
                    ┌─────▼─────┐
                    │ MCP Server │  ← ai-workspace serve
                    │  (stdio)   │
                    └─────┬─────┘
                          │
                    ┌─────▼─────┐
                    │  AI Agent  │  sees files, dirs & notes
                    │            │  from all 3 projects
                    └────────────┘
```

Group projects together. Share files and notes. Your agent sees everything.

## Quick Start

### Install

```bash
cargo install --path .
```

Requires Rust 1.85+ (edition 2024). SQLite is bundled — no extra dependencies.

### Set up projects

```bash
# Register two projects in the same group
cd ~/api
ai-workspace init --group backend

cd ~/web
ai-workspace init --group backend
```

### Share context

```bash
cd ~/api
ai-workspace share src/schema.rs --label "db schema"
ai-workspace note "Deploy: run migrations before release" --group backend
```

Now any agent working in `~/web` can read `schema.rs` and the deploy note from `~/api`.

### Connect to your agent

**Claude Code:**

```bash
claude mcp add --scope user ai-workspace -- ai-workspace serve
```

**Other MCP clients** (Cursor, Windsurf, Claude Desktop, etc.):

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

That's it. The agent now has access to 5 MCP tools: `workspace_context`, `workspace_read`, `workspace_search`, `list_groups`, and `list_projects`.

## CLI at a glance

| Command | What it does |
|---------|-------------|
| `init --group <name>` | Register project, join/create a group |
| `share <path> --label <label>` | Share a file or directory |
| `note <text> --group <name>` | Add a group-scoped note |
| `edit <target> --content/--label/--scope` | Edit a shared item |
| `rm <target>` | Remove a shared item |
| `leave <group>` | Remove project from a group |
| `delete-group <group>` | Delete a group entirely |
| `status` | Show project info, groups, and items |
| `export` | Export project config to `.ai-workspace.json` |
| `sync` | Clean up stale files + reconcile `.ai-workspace.json` |
| `serve` | Start the MCP server |

## Team Sharing

Share your workspace config with your team by committing `.ai-workspace.json` to the repo:

```bash
# One-time: export current config
ai-workspace export

# Commit the file
git add .ai-workspace.json
git commit -m "chore: add shared workspace config"
```

When a teammate clones the repo and runs `init`, they automatically get the same groups, shared files, and notes:

```bash
cd ~/cloned-repo
ai-workspace init
# → picks up name, groups, shares, and notes from .ai-workspace.json
```

The `--name` flag overrides the name from `.json`, and `--group` is additive. Running `sync` also reconciles the database with `.ai-workspace.json` if present.

## Documentation

| Guide | Description |
|-------|-------------|
| [Getting Started](docs/getting-started.md) | Concepts, scopes, visibility rules, data storage |
| [CLI Reference](docs/cli.md) | All commands and options in detail |
| [MCP Server](docs/mcp-server.md) | MCP tools, protocol, and integration guide |

## Installation

<details>
<summary><b>macOS (Apple Silicon)</b></summary>

```bash
curl -fsSL https://github.com/lee-to/ai-workspace/releases/latest/download/ai-workspace-aarch64-apple-darwin.tar.gz | sudo tar xz -C /usr/local/bin
```
</details>

<details>
<summary><b>macOS (Intel)</b></summary>

```bash
curl -fsSL https://github.com/lee-to/ai-workspace/releases/latest/download/ai-workspace-x86_64-apple-darwin.tar.gz | sudo tar xz -C /usr/local/bin
```
</details>

<details>
<summary><b>Linux (x86_64)</b></summary>

```bash
curl -fsSL https://github.com/lee-to/ai-workspace/releases/latest/download/ai-workspace-x86_64-unknown-linux-gnu.tar.gz | sudo tar xz -C /usr/local/bin
```
</details>

<details>
<summary><b>Linux (aarch64)</b></summary>

```bash
curl -fsSL https://github.com/lee-to/ai-workspace/releases/latest/download/ai-workspace-aarch64-unknown-linux-gnu.tar.gz | sudo tar xz -C /usr/local/bin
```
</details>

<details>
<summary><b>Windows (x86_64, PowerShell)</b></summary>

```powershell
Invoke-WebRequest -Uri "https://github.com/lee-to/ai-workspace/releases/latest/download/ai-workspace-x86_64-pc-windows-msvc.zip" -OutFile ai-workspace.zip
Expand-Archive ai-workspace.zip -DestinationPath "$env:USERPROFILE\bin" -Force
```

Add `%USERPROFILE%\bin` to `PATH` if needed.
</details>

<details>
<summary><b>Build from source</b></summary>

```bash
git clone https://github.com/lee-to/ai-workspace.git
cd ai-workspace
cargo install --path .
```
</details>

Requires Rust 1.85+ (edition 2024). SQLite is bundled — no extra dependencies.

## License

MIT
