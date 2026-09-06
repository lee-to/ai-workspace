# Architecture: Layered Architecture

## Overview
This project uses a layered architecture where each layer has a clear responsibility and depends only on the layers below it. The `ai-workspace` binary has local CLI/stdio MCP entry points backed by SQLite plus optional cloud push/HTTP MCP entry points backed by PostgreSQL snapshots.

This pattern was chosen because the project is a single-binary CLI/MCP tool with an embedded database and low domain complexity. Layered architecture keeps the code simple, navigable, and easy to extend without the ceremony of dependency inversion or bounded contexts.

## Decision Rationale
- **Project type:** CLI tool + local/hosted MCP server (single binary)
- **Tech stack:** Rust, SQLite, PostgreSQL, Clap, Axum, serde
- **Key factor:** Local-first behavior remains isolated from the optional synchronized cloud boundary

## Folder Structure
```
src/
├── main.rs             # Entry point: parses CLI args, dispatches to cli or mcp
├── models.rs           # Shared data types (Project, Group, SharedItem, SharedItemKind)
├── codegraph.rs        # Rust CodeGraph extraction/sync service
├── indexer.rs          # Markdown FTS indexing service
├── walk.rs             # Shared file walking and grep policy
├── cli/
│   └── mod.rs          # Presentation layer: CLI subcommands and handlers
├── cloud/
│   ├── auth.rs         # OIDC/JWKS JWT validation
│   ├── client.rs       # Blocking cloud push client
│   ├── http.rs         # Axum server, health, metadata, snapshot endpoint
│   ├── mcp.rs          # Hosted stateless MCP allowlist adapter
│   ├── models.rs       # Versioned cloud wire records and validation
│   ├── snapshot.rs     # Deterministic local snapshot builder
│   └── store.rs        # PostgreSQL snapshot/document read model
├── db/
│   ├── mod.rs          # Data layer: module exports (Db)
│   ├── schema.rs       # Schema creation (tables, FTS5, triggers)
│   └── crud.rs         # CRUD operations (Db struct with all queries)
└── mcp/
    ├── mod.rs          # Presentation layer: MCP stdio server loop
    ├── protocol.rs     # JSON-RPC request/response types
    └── tools.rs        # MCP tool implementations (call db layer)
migrations/
└── 0001_cloud_read_model.sql # PostgreSQL tables, FTS, forced tenant RLS
```

## Dependency Rules

```
  ┌──────────┐     ┌──────────┐
  │   CLI    │     │   MCP    │    ← Presentation layer
  └────┬─────┘     └────┬─────┘
       │                │
       ├───────┬────────┤
       ▼       ▼        ▼
  ┌────────┐ ┌───────────┐
  │Indexer │ │ CodeGraph │        ← Shared service modules
  └────┬───┘ └─────┬─────┘
       └──────┬────┘
              ▼
        ┌──────────┐
        │    Db    │             ← Data layer
        └────┬─────┘
             ▼
        ┌──────────┐
        │  Models  │             ← Shared types
        └──────────┘
```

- ✅ `cli` → `db`, `models` (CLI handlers call Db methods, use model types)
- ✅ `mcp` → `db`, `models` (MCP tools call Db methods, serialize model types)
- ✅ `cli`/`mcp` → `codegraph`, `indexer`, `walk` (presentation layers may call shared service modules)
- ✅ `codegraph`/`indexer` → `db`, `models`, `walk` (services enforce file policy and use Db APIs)
- ✅ `cloud::snapshot`/`cloud::client` → local `db`, cloud wire models (explicit outbound sync)
- ✅ `cloud::http`/`cloud::mcp` → `cloud::auth`, `cloud::store`, cloud wire models (hosted boundary)
- ✅ `cloud::store` → PostgreSQL only; tenant access is transaction-scoped and protected by forced RLS
- ✅ `db` → `models` (CRUD returns model structs)
- ❌ `db` → `cli` or `mcp` (data layer must not know about presentation)
- ❌ `cli` → `mcp` or `mcp` → `cli` (presentation layers are independent)
- ❌ `models` → anything (pure data types, no dependencies)

## Layer/Module Communication
- **CLI → Db**: CLI handlers create a `Db` instance and call methods directly (`db.add_project()`, `db.share_file()`)
- **MCP → Db**: MCP tool handlers create a `Db` instance and call the same methods, serializing results as JSON-RPC responses
- **CLI/MCP → CodeGraph**: CLI runs indexing/sync orchestration; MCP reads graph query APIs and can request bounded source snippets through `codegraph.rs`
- **Models as shared language**: Both layers use the same `Project`, `Group`, `SharedItem` types — no DTOs needed at this scale
- **Cloud synchronization**: The CLI builds a deterministic wire snapshot from local SQLite and explicitly shared files, then sends it over HTTPS. The hosted service validates and atomically replaces one project's PostgreSQL projection.
- **Hosted MCP**: HTTP MCP reads only token-derived tenant snapshots through a seven-tool positive allowlist. It never routes into local SQLite/filesystem handlers.

## Key Principles
1. **Models are plain data** — `models.rs` contains only structs, enums, and their serialization. No business logic, no database code.
2. **Storage stays behind its module** — SQLite access goes through `db::Db`; PostgreSQL access goes through `cloud::store::CloudStore`. Presentation handlers contain no raw SQL.
3. **Presentation layers are independent** — CLI and MCP never import from each other. They share behavior only through the Db layer.
4. **Error handling follows the layer** — `db` returns `anyhow::Result`, CLI propagates to user-facing messages, MCP converts to JSON-RPC error codes.
5. **New features flow top-down** — Add model types → add Db methods → expose via CLI and/or MCP.

## Code Examples

### Adding a new Db operation
```rust
// src/db/crud.rs
impl Db {
    pub fn list_items_by_label(&self, label: &str) -> Result<Vec<SharedItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, path, content, label, project_id, group_id,
                    created_by_project_id, created_at, updated_at
             FROM shared_items WHERE label = ?1"
        )?;
        let items = stmt.query_map(params![label], |row| {
            // map row to SharedItem
        })?;
        Ok(items.collect::<Result<Vec<_>, _>>()?)
    }
}
```

### Exposing via CLI
```rust
// src/cli/mod.rs
Command::ListByLabel { label } => {
    let db = Db::open()?;
    let items = db.list_items_by_label(&label)?;
    for item in items {
        println!("{}: {} ({})", item.id, item.label.unwrap_or_default(), item.kind);
    }
    Ok(())
}
```

### Exposing via MCP
```rust
// src/mcp/tools.rs
"list_by_label" => {
    let label = params.get("label")
        .and_then(|v| v.as_str())
        .ok_or_else(|| McpError::invalid_params("label is required"))?;
    let db = Db::open()?;
    let items = db.list_items_by_label(label)?;
    Ok(serde_json::to_value(items)?)
}
```

## Anti-Patterns
- ❌ **SQL in presentation layers** — Never write raw SQL in `cli/` or `mcp/`. All queries belong in `db/crud.rs`.
- ❌ **Cross-referencing presentation layers** — `cli` must never import from `mcp` or vice versa.
- ❌ **Business logic in models** — `models.rs` is for data shapes and serialization only. Validation and computed fields belong in `db` or the calling layer.
- ❌ **Db knowing about output format** — `db::Db` returns Rust types, never JSON strings or formatted CLI output.
- ❌ **Skipping the Db layer** — Even for "simple" queries, go through `Db` to keep all schema knowledge in one place.
- ❌ **Routing hosted calls through local MCP tools** — Hosted MCP uses synchronized PostgreSQL data and its explicit read-only allowlist only.
- ❌ **Weakening tenant isolation in application code** — Runtime roles must remain non-owner/non-superuser/non-`BYPASSRLS`; every tenant transaction sets `app.workspace_id`.
