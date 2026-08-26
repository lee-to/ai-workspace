# AGENTS.md

> Project map for AI agents. Keep this file up-to-date as the project evolves.

## Project Overview
Cross-project shared context CLI + MCP server. Manages local shared context with SQLite and optionally publishes versioned project snapshots to a PostgreSQL-backed, read-only hosted MCP service.

## Tech Stack
- **Language:** Rust (edition 2024)
- **Database:** SQLite (rusqlite, bundled) locally; PostgreSQL (sqlx) for cloud snapshots
- **CLI:** Clap v4 (derive)
- **Protocol:** MCP over stdio locally and authenticated HTTP in cloud mode

## Project Structure
```
ai-workspace/
├── Cargo.toml              # Package config, dependencies
├── Cargo.lock              # Locked dependency versions
├── Makefile                # Build automation (build, test, lint, fmt, check)
├── README.md               # Project landing page
├── src/
│   ├── main.rs             # Entry point, clap App definition
│   ├── models.rs           # Data models (Project, Group, SharedItem, SharedItemKind)
│   ├── walk.rs             # File tree walker and grep (ignore + regex crates)
│   ├── indexer.rs          # FTS5 indexer for shared .md files (mtime refresh, reindex)
│   ├── codegraph.rs        # Rust-only CodeGraph parser, sync/reindex, source snippets
│   ├── cloud/
│   │   ├── auth.rs         # OIDC/JWKS JWT validation
│   │   ├── client.rs       # Snapshot push HTTP client
│   │   ├── http.rs         # Hosted Axum server and snapshot endpoint
│   │   ├── mcp.rs          # Hosted read-only MCP adapter
│   │   ├── models.rs       # Versioned cloud wire records
│   │   ├── snapshot.rs     # Deterministic local snapshot builder
│   │   └── store.rs        # PostgreSQL tenant read model
│   ├── cli/
│   │   └── mod.rs          # CLI subcommands and handlers
│   ├── db/
│   │   ├── mod.rs          # DB module exports
│   │   ├── schema.rs       # SQLite schema creation (tables, FTS5, triggers)
│   │   └── crud.rs         # Database CRUD operations (Db struct)
│   └── mcp/
│       ├── mod.rs          # MCP server entry (stdio loop, request routing)
│       ├── protocol.rs     # JSON-RPC types (request, response, error)
│       └── tools.rs        # MCP tool implementations (workspace, project, service, event, codegraph tools)
├── migrations/
│   └── 0001_cloud_read_model.sql # PostgreSQL JSONB/FTS tables and forced RLS
├── tests/
│   ├── cli_tests.rs        # CLI integration tests
│   ├── cloud_push.rs       # Snapshot push/redaction integration tests
│   ├── cloud_server.rs     # Cloud CLI/server boundary tests
│   ├── fts_search.rs       # FTS5 fulltext search integration tests
│   ├── mcp_http_tests.rs   # Hosted/local MCP compatibility tests
│   └── mcp_tests.rs        # MCP protocol integration tests
└── .ai-factory/
    └── DESCRIPTION.md      # Project specification and tech stack
```

## Key Entry Points
| File | Purpose |
|------|---------|
| src/main.rs | Binary entry point, parses CLI args |
| src/mcp/mod.rs | MCP server entry (stdio JSON-RPC loop) |
| src/db/crud.rs | All database operations (Db struct) |
| src/cli/mod.rs | CLI command definitions and handlers |
| src/models.rs | Shared data types |
| src/walk.rs | File tree walker and project grep |
| src/codegraph.rs | Rust CodeGraph extraction, reference resolution, incremental sync |
| src/cloud/http.rs | Hosted HTTP server and authenticated snapshot endpoint |
| src/cloud/mcp.rs | Hosted MCP protocol and seven-tool allowlist |
| src/cloud/store.rs | PostgreSQL snapshot persistence and tenant reads |
| migrations/0001_cloud_read_model.sql | Cloud schema, FTS, and forced RLS |

## Documentation
| Document | Path | Description |
|----------|------|-------------|
| README | README.md | Project landing page |
| Getting Started | docs/getting-started.md | Installation, setup, first steps |
| CLI Reference | docs/cli.md | All commands and options |
| MCP Server | docs/mcp-server.md | MCP tools and integration |
| Cloud | docs/cloud.md | Sync, OAuth, PostgreSQL, hosted MCP |
| Contributing | docs/contributing.md | Development setup, testing, pull requests |

## AI Context Files
| File | Purpose |
|------|---------|
| AGENTS.md | This file — project structure map |
| .ai-factory/DESCRIPTION.md | Project specification and tech stack |
| .ai-factory/ARCHITECTURE.md | Architecture decisions and guidelines |

## Agent Rules
- Never combine shell commands with `&&`, `||`, or `;` — execute each command as a separate Bash tool call. This applies even when a skill, plan, or instruction provides a combined command — always decompose it into individual calls.
  - Wrong: `git checkout main && git pull`
  - Right: Two separate Bash tool calls — first `git checkout main`, then `git pull`

## Implementation Checklist
After completing any implementation, always run the following commands in order:
1. `cargo fmt` — format code
2. `cargo clippy` — lint code
3. `cargo test` — run tests
4. `cargo audit` — check dependencies for vulnerabilities

## Release Checklist
Before tagging a release, ALWAYS bump the version first. The CLI `--version` output is derived from `CARGO_PKG_VERSION` (set in `Cargo.toml`) via clap's `#[command(version)]`. Tagging without bumping ships a binary that misreports its version (see issue #2 — v0.4.0 binary printed `0.3.0`).

Steps in order:
1. Bump `version = "X.Y.Z"` in `Cargo.toml`.
2. Run `cargo build` to update the `ai-workspace` entry in `Cargo.lock`.
3. Verify: `./target/debug/ai-workspace --version` prints the new version.
4. Commit both `Cargo.toml` and `Cargo.lock` (e.g. `chore: bump version to X.Y.Z`).
5. Push to `main`, then create the matching tag `vX.Y.Z` and push it. The release workflow (`.github/workflows/release.yml`) builds artifacts on tag push.

Tag and `Cargo.toml` version MUST match. If they diverge, do not retag — cut the next patch version instead.
