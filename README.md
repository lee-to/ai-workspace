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
                    ┌─────▼──────┐
                    │  "backend" │  ← group
                    │   group    │
                    └─────┬──────┘
                          │
                    ┌─────▼──────┐
                    │ MCP Server │  ← ai-workspace serve
                    │  (stdio)   │
                    └─────┬──────┘
                          │
                    ┌─────▼──────┐
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

Requires Rust 1.88+ (edition 2024). SQLite is bundled — no extra dependencies.

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

That's it. The agent now has access to 11 MCP tools: `workspace_context`, `workspace_read`, `workspace_search`, `workspace_search_fulltext`, `workspace_service_graph`, `workspace_events`, `workspace_event_details`, `list_groups`, `list_projects`, `project_tree`, and `project_grep`.
By default, project navigation, full-text file search, and direct path reads hide dotfiles and credential-like paths such as `.env`, `.ssh`, `.aws`, `*.pem`, and `*.key`. `workspace_read`, `project_tree`, and `project_grep` support explicit opt-in flags; `workspace_search_fulltext` is stricter and never returns hidden or credential-like `.md` paths.

By default, MCP tools expose only files, directories, and notes that you explicitly share. Full project tree, grep, path reads, and absolute project path metadata require opting in with `AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1` on the MCP server process.

You can also restrict the MCP server to part of the local workspace:

```bash
ai-workspace serve --scope current-project
ai-workspace serve --group backend
ai-workspace serve --project api
```

The same modes are available through `AI_WORKSPACE_SCOPE=global|current-project|group|project`, `AI_WORKSPACE_SCOPE_GROUP`, and `AI_WORKSPACE_SCOPE_PROJECT`. CLI flags override env vars. Scoping filters metadata, search, service graphs, events, reads, tree, and grep; `AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS=1` only broadens filesystem access inside the configured MCP scope.

## Example Prompts

Once connected, you can talk to your AI agent naturally. Here are some examples:

**Discover context:**
- *"What shared context do I have from other projects?"*
- *"Show me all projects and groups in my workspace"*
- *"What files are shared in the backend group?"*

**Read shared files:**
- *"Read the shared database schema from the api project"*
- *"Show me the deploy guide shared by the infra team"*
- *"What's in the shared config directory?"*

**Search notes:**
- *"Search workspace notes for migration instructions"*
- *"Are there any shared notes about the deploy process?"*
- *"Find notes mentioning staging environment"*

**Navigate shared project files:**
- *"Show me the shared file tree of the api project"*
- *"List the files under the shared src/ directory in project 2"*
- *"Search the api project's shared files for any function that mentions 'auth'"*
- *"Grep the web project's shared files for TODO comments"*

**Cross-project tasks:**
- *"I'm building a new endpoint — check the shared API schema and follow the same patterns"*
- *"Before I refactor this model, check if other projects share files that depend on it"*
- *"Read the shared style guide and apply it to this component"*

The agent will automatically call the right MCP tools (`workspace_context`, `workspace_read`, `workspace_search`, `workspace_search_fulltext`, `workspace_service_graph`, `workspace_events`, `workspace_event_details`) to answer these.

## CLI at a glance

| Command | What it does |
|---------|-------------|
| `init --slug <slug> --group <name>` | Register project, set a stable slug, join/create a group, auto-share key files |
| `init --preset ai-factory` | Create and share baseline `.ai-factory` context files |
| `share <path> --label <label>` | Share a file or directory |
| `note <text> --group <name>` | Add a group-scoped note |
| `edit <target> --content/--label/--scope` | Edit a shared item |
| `rm <target>` | Remove a shared item |
| `leave <group>` | Remove project from a group |
| `delete-group <group>` | Delete a group entirely |
| `link add <from> <to> --kind depends_on` | Connect services with a directional relationship |
| `link list [--project <slug>]` | Inspect service links and group graphs |
| `artifact depends <item> <slug> --kind references --reaction update` | Mark shared artifacts that depend on a service |
| `artifact deps [item]` | List artifact dependency metadata |
| `event create --kind service_changed --source <slug>` | Create service events and calculate affected projects/artifacts |
| `event inbox` | Show open events affecting the current project |
| `destroy [target]` | Remove current or targeted project from ai-workspace (keeps files) |
| `status` | Show project info, groups, and items |
| `export` | Export project config to workspace JSON |
| `sync` | Clean up stale files + reconcile workspace JSON |
| `search <query>` | Full-text search over shared `.md` files (FTS5, bm25-ranked) |
| `reindex` | Rebuild the full-text index for all shared `.md` files |
| `serve [--scope ...] [--group ...] [--project ...]` | Start the MCP server, optionally scoped to a project or group |
| `update` | Update to the latest version |

## Team Sharing

Share your workspace config with your team by committing `.ai-workspace.json` to the repo:

```bash
# One-time: export current config
ai-workspace export

# Commit the file
git add .ai-workspace.json
git commit -m "chore: add shared workspace config"
```

When a teammate clones the repo and runs `init`, they automatically get the same groups, stable slug, shared files/directories, notes, and artifact dependency metadata:

```bash
cd ~/cloned-repo
ai-workspace init
# → picks up name, slug, groups, shares, notes, and dependencies from the configured workspace JSON
```

The `--name` flag overrides the name from `.json`, and `--group` is additive. Running `sync` also reconciles the database with the configured workspace JSON if present. Shared paths from config must exist and resolve inside the project directory. They are literal file or directory paths, not glob patterns; use `"docs"` rather than `"docs/**"` to share a directory. The workspace JSON exports project-scoped configuration only: group notes and event history stay local and are intentionally not exported.

If your repo keeps AI-related files under a dedicated directory, pass a custom config path. The path must be relative and remain inside the project root; absolute paths, `..`, backslashes on Unix, symlink escapes, and final config-path symlinks are rejected. Existing files at that path are only updated when they are already recognizable ai-workspace configs, so ordinary files such as `README.md` or `package.json` are not overwritten by a mistaken config path:

```bash
ai-workspace --config .ai/ai-workspace.json export
AI_WORKSPACE_CONFIG=.ai/ai-workspace.json ai-workspace init
```

## Documentation

| Guide | Description |
|-------|-------------|
| [Getting Started](docs/getting-started.md) | Concepts, scopes, visibility rules, data storage |
| [CLI Reference](docs/cli.md) | All commands and options in detail |
| [MCP Server](docs/mcp-server.md) | MCP tools, protocol, and integration guide |
| [Contributing](docs/contributing.md) | Development setup, testing, pull requests |

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

Requires Rust 1.88+ (edition 2024). SQLite is bundled — no extra dependencies.

### Update

```bash
ai-workspace update
```

Downloads the latest release from GitHub and replaces the current binary. No Rust or Cargo required.

## License

MIT
