# Research

Updated: 2026-08-25 18:48 MSK
Status: active

## Active Summary (input for $aif-plan)
<!-- aif:active-summary:start -->
Topic: Hybrid local and cloud architecture for ai-workspace

Goal:
- Preserve the current offline local workflow while making shared workspace context available to a team through a hosted MCP endpoint.

Constraints:
- The existing `Db` is built directly around `rusqlite::Connection` and a local SQLite file (`src/db/crud.rs`).
- The SQLite schema uses `PRAGMA`, FTS5 virtual tables, `rowid`, SQLite triggers, and local autoincrement IDs (`src/db/schema.rs`).
- Several MCP tools read the registered project filesystem directly, so a cloud PostgreSQL database cannot by itself provide access to files stored on a developer machine (`src/mcp/tools.rs`).
- The current MCP server is a custom newline-delimited JSON-RPC stdio implementation and advertises protocol version `2024-11-05` (`src/mcp/mod.rs`).
- Cloud access must preserve the existing scope and sensitive-path protections and add user, team, and tenant authorization.

Decisions:
- Keep SQLite as the local working database and offline cache; do not replace it with PostgreSQL or introduce a generic dual-database abstraction for the existing `Db` API.
- Add a separate cloud control plane backed by PostgreSQL and connect the local CLI to it through an HTTPS synchronization API.
- Keep local files, absolute project paths, mtimes, local FTS5 data, and the initial CodeGraph local-only.
- Sync logical project identities, groups, memberships, shared-item metadata, notes, service links, dependencies, and events.
- Upload shared Markdown content only through an explicit opt-in policy. Never upload hidden or credential-like content by default.
- Rebuild derived search indexes independently: SQLite FTS5 locally and PostgreSQL full-text search in the cloud.
- Expose the hosted MCP server over Streamable HTTP with token-derived tenant/workspace scope. Keep the existing stdio transport for local clients.
- Start with a read-only cloud mirror. The smallest useful version is Git-backed import where practical, or an explicit `cloud push` of project config, notes, graph metadata, events, and opted-in Markdown snapshots.
- Initially expose only cloud-safe tools such as workspace context, synced note/Markdown reads and search, service graph, and events. Do not expose remote local-file writes, unrestricted tree/grep, or cloud CodeGraph in the first release.
- If bidirectional offline sync becomes necessary, use global UUIDs, device IDs, idempotent operation IDs, server revisions, pull cursors, deletion tombstones, and optimistic concurrency. Surface conflicts instead of silently applying last-write-wins.

Open questions:
- Is Git-backed import sufficient for the first team workflow, or must uncommitted notes and events synchronize immediately?
- Which content may leave a developer machine: metadata only, shared Markdown, or selected source files?
- Which MCP clients must be supported, and which protocol compatibility versions do they currently require?
- What team roles and permissions are required beyond owner, editor, and reader?
- Is background synchronization required, or is an explicit CLI command acceptable for the first release?

Success signals:
- Existing local commands and stdio MCP continue to work offline without a cloud account.
- A team member can authenticate to a hosted MCP endpoint and see only the authorized workspace's synchronized context.
- A local push or Git import makes opted-in shared context searchable from the hosted MCP without exposing local absolute paths or sensitive files.
- Repeated synchronization is idempotent, deletions propagate safely, and stale writes cannot silently overwrite newer shared data.
- Tenant isolation is verified at both the application and database policy boundaries.

Next step:
- Use `$aif-plan full hybrid-local-cloud-ai-workspace` to plan a read-only cloud mirror first, including the synchronization contract, PostgreSQL cloud schema, authentication boundary, Streamable HTTP MCP compatibility, and an end-to-end local-to-cloud test.
<!-- aif:active-summary:end -->

## Sessions
<!-- aif:sessions:start -->
### 2026-08-25 18:48 MSK — Hybrid local and cloud architecture
What changed:
- Established the recommended split between the existing local SQLite runtime and a new PostgreSQL-backed cloud control plane.
- Selected a read-only cloud mirror as the first release and deferred general bidirectional replication.

Key notes:
- A direct SQLite/PostgreSQL backend switch would not solve access to developer-local files and would require rewriting SQLite-specific storage and search behavior.
- Cloud MCP can serve only synchronized content unless a later local agent or tunnel is introduced.
- Current MCP transport and protocol compatibility require separate work from database synchronization.
- Research Coherence Gate: passed. The Active Summary is self-contained, consistent with durable research, and separates evidence, decisions, and unknowns.

Links (paths):
- `src/db/crud.rs`
- `src/db/schema.rs`
- `src/mcp/mod.rs`
- `src/mcp/tools.rs`
- `src/models.rs`
- `.ai-factory/ARCHITECTURE.md`
- https://modelcontextprotocol.io/specification/2026-07-28
- https://modelcontextprotocol.io/specification/draft/basic/transports
- https://modelcontextprotocol.io/specification/draft/basic/authorization
- https://www.postgresql.org/docs/current/textsearch-controls.html
- https://www.postgresql.org/docs/17/ddl-rowsecurity.html
<!-- aif:sessions:end -->
