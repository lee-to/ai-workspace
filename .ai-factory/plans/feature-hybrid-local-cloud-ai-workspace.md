# Implementation Plan: Hybrid Local and Cloud ai-workspace

Branch: feature/hybrid-local-cloud-ai-workspace
Created: 2026-08-25

## Original Request

hybrid-local-cloud-ai-workspace

## Settings

- Testing: yes
- Logging: verbose
- Docs: yes

## Research Context

Source: `.ai-factory/RESEARCH.md` (Active Summary, Updated: 2026-08-25 18:48 MSK, SHA256: 63787d64414cdfa03e41e3389fb642829625cb7801a1d91af625bf3b5ad99e63)

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

## Resolved Scope and Contracts

### Product slice

- Keep one `ai-workspace` binary. Add `ai-workspace cloud push` for the local client and `ai-workspace cloud serve` for the hosted service; do not split the repository into a workspace or introduce a shared storage trait.
- `cloud push` publishes only the current registered project. It never calls local `sync`, never deletes local data, and never runs automatically.
- Use an explicit push rather than Git import for v1 so project notes, group notes created by the project, service links, dependencies, and source events are included. Git import remains a later alternative.
- The cloud is a derived, read-only mirror. The only cloud mutation endpoint in v1 is the authenticated project snapshot push API.
- Markdown bodies are excluded unless the user passes `--include-markdown`. That flag includes only safe, explicitly shared `.md` files and directory children; hidden, sensitive, symlink-escaped, oversized, and non-UTF-8 files remain excluded.

### Snapshot contract

- Add a versioned `CloudProjectSnapshot` wire model containing logical slugs/names and normalized relative paths only. Never serialize existing local models directly because they contain local `i64` IDs and absolute paths.
- Give every remotely addressable object a deterministic, versioned `cloud_key` built from its logical coordinates, never from a local row ID. Use readable natural keys for projects/files/links and a SHA-256 fingerprint plus duplicate ordinal where notes/events have no stable natural ID: `project:<slug>`, `share:<project-slug>:<rel-path>`, `file:<project-slug>:<rel-path>`, `note:<project-slug>:<scope>:<fingerprint>:<ordinal>`, `link:<from-slug>:<to-slug>:<kind>`, `dependency:<project-slug>:<share-path>:<target-slug>:<kind>`, and `event:<source-slug>:<fingerprint>:<ordinal>`. Event fingerprints exclude mutable status fields so close/reopen does not change the key. Centralize key construction and collision ordinals in `cloud::models`; do not let handlers invent identifiers.
- Partition ownership by `(workspace_id, project_slug)`. A successful push atomically replaces only that project's mirror partition, so removed notes, shares, documents, links, dependencies, and project-owned events disappear without tombstones.
- Include project name/slug, group names, project notes, group notes created by the project, project shares, optional Markdown documents, artifact dependencies keyed by share path, outgoing service links, and events sourced by the project. Resolve related projects by slug; unresolved/deleted event targets remain `null` rather than triggering a local migration.
- Deterministically sort every collection before serialization and hash the canonical JSON. Identical content is a server-side no-op and returns the current revision.
- Keep a local `cloud_sync_state` row keyed by normalized endpoint, workspace slug, and project slug. It stores only the last accepted revision/hash, never credentials or content.
- The push request carries `base_revision`, `force`, and the snapshot. A missing base revision creates only when the remote partition is absent; a mismatched revision returns `409` with current revision/hash. `--force` is the only explicit overwrite escape hatch. If a retry has an old base revision but the same content hash as the current remote snapshot, return success without incrementing the revision.
- Preserve the existing 1 MiB per-Markdown-file limit. Cap a snapshot at 1,000 documents and 16 MiB serialized JSON; exceeding either aggregate limit fails the complete push instead of truncating it.

### Cloud storage and tenancy

- Use PostgreSQL only for the hosted service. Add a small read model: `cloud_workspaces`, `cloud_project_snapshots` (`JSONB`, revision, hash, audit fields), and `cloud_documents` (note/Markdown projection plus generated `tsvector`).
- Enable and force PostgreSQL Row-Level Security on every tenant table. Each authenticated transaction sets a transaction-local `app.workspace_id`; SQL also includes explicit workspace predicates.
- Delegate users and team membership to the external authorization server for v1. Validated access tokens must contain `sub`, `workspace_id`, `workspace_slug`, and OAuth scopes. Require `ai-workspace:push` for snapshots and `ai-workspace:read` for hosted MCP. Do not accept workspace or subject headers/tool arguments as authorization evidence.
- Implement an OAuth resource server only: validate JWT signature, issuer, audience, time claims, workspace claims, and scopes against configured OIDC/JWKS metadata. Token issuance, login UI, refresh-token storage, DCR, and team administration stay outside this repository.
- Terminate TLS and apply rate limiting at the deployment ingress. The service binds HTTP internally and publishes RFC 9728 Protected Resource Metadata for MCP authorization discovery.

### Hosted MCP contract

- Leave local `ai-workspace serve`, stdio framing, MCP `2024-11-05`, local scopes, and its complete local tool catalog unchanged.
- Hosted `/mcp` targets MCP `2026-07-28` stateless Streamable HTTP with one JSON response per `POST`; do not add SSE, sessions, `Mcp-Session-Id`, or a legacy hosted transport.
- Validate `MCP-Protocol-Version`, `Mcp-Method`, `Mcp-Name`, matching body metadata, request content type, body limit, and `Origin` when present. Implement `server/discover`, `tools/list`, and `tools/call` according to the selected protocol schema.
- Use a positive remote allowlist: `workspace_context`, `workspace_read`, `workspace_search`, `workspace_search_fulltext`, `workspace_service_graph`, `workspace_events`, and `workspace_event_details`.
- Remote `workspace_read` accepts only stable cloud project/document keys and reads stored note/Markdown snapshots. Always reject `project_file_write`, `project_tree`, `project_grep`, every `codegraph_*` tool, arbitrary filesystem path mode, and hidden/sensitive opt-in flags.

### Cloud object lookup contract

- `workspace_context` reads all `cloud_project_snapshots` visible under RLS and returns project slugs, group names, share keys, and link/dependency keys from the JSONB projections.
- `workspace_search` and `workspace_search_fulltext` query `cloud_documents` by kind and return `document_key`, `project_slug`, scope/group metadata, optional relative path, snippet, and rank.
- `workspace_read` accepts a `document_key` returned by context/search and performs an indexed `(workspace_id, document_key)` lookup in `cloud_documents`; it never accepts a local item ID or arbitrary path.
- `workspace_service_graph` assembles deterministic link keys from the visible project snapshots.
- `workspace_events` returns deterministic `event_key` values. `workspace_event_details` accepts only that key, derives the source project slug from the key, loads the corresponding project snapshot, and resolves the exact event within its event array.
- Keep these lookups intentionally snapshot-backed for v1. Add normalized event/link tables only after measured workspace size or query latency makes the JSONB read path insufficient.

## Commit Plan

- **Commit 1** (after tasks 1-3): `feat: add versioned cloud snapshot client`
- **Commit 2** (after tasks 4-6): `feat: add postgres cloud mirror service`
- **Commit 3** (after tasks 7-9): `feat: expose read-only hosted mcp`
- **Commit 4** (after tasks 10-11): `docs: document and verify cloud mirror`

## Tasks

### Phase 1: Local Snapshot and Push Client

- [x] Task 1: Define the versioned cloud wire and public-key contracts.

  Deliverable and expected behavior:
  - Create `src/cloud/mod.rs` and `src/cloud/models.rs` with `CloudProjectSnapshot`, nested wire-only records, push request/response/error types, schema version `1`, deterministic ordering requirements, and validation for slugs, normalized relative paths, counts, and aggregate size.
  - Define the complete `cloud_key` grammar and canonical fingerprint inputs for projects, shares, documents, notes, links, dependencies, events, event targets, and event artifacts. Event keys exclude mutable status; duplicate logical records receive a deterministic ordinal after sorting.
  - Keep this task dependency-free beyond crates already present. Transport, PostgreSQL, auth, UUID, and hashing crates are added only by the tasks that first use them.
  - Register `mod cloud` in `src/main.rs` without changing any existing command behavior.
  - Unit-test serialization/validation shapes so the payload cannot contain absolute paths, local database IDs, mtimes, FTS rows, CodeGraph data, or credentials, and every remotely addressable record requires a cloud key.

  Files: `src/main.rs`, `src/cloud/mod.rs`, `src/cloud/models.rs`.

  Logging requirements:
  - Pure model serialization emits no logs.
  - Callers log validation failures at `DEBUG` with category/count and at `WARN` for rejected snapshots; never log serialized note/file content or tokens.
  - Keep logging controlled by the existing `RUST_LOG`/`env_logger` setup.

  Dependencies: none.

- [x] Task 2: Build a deterministic, policy-safe current-project snapshot.

  Deliverable and expected behavior:
  - Create `src/cloud/snapshot.rs` with `build_project_snapshot(&Db, &Project, include_markdown)`.
  - Reuse existing `Db` readers for group membership, owned items, project-created group notes, outgoing service links, artifact dependencies, source events, targets, artifacts, and group snapshots; add only missing read-only queries to `src/db/crud.rs` and exports to `src/db/mod.rs`.
  - Extract only reusable pure Markdown normalization, canonicalization, size, and UTF-8 reading primitives from `src/indexer.rs`/`src/walk.rs`; keep local and cloud selection policies separate. The local indexer must retain its existing explicitly shared `.ai-factory` behavior through `path_allowed_for_shared_ai_factory`, while the cloud collector must use `path_allowed_by_options(..., WalkOptions::default())` so every hidden or sensitive path remains excluded even when explicitly shared. The cloud collector expands safe explicitly shared directories, respects gitignore rules, accepts `.md` only, and never mutates the FTS index.
  - Without `--include-markdown`, include metadata and notes but no filesystem bodies. With it, include only safe explicitly shared Markdown.
  - Sort all snapshot arrays and compute a stable SHA-256 over canonical JSON. Fail before network access when the 1,000-document or 16 MiB aggregate ceiling is exceeded.
  - Add `sha2` only here and implement the centralized key/fingerprint constructors defined in Task 1. Verify key stability across repeated snapshots and status-only event changes.
  - Add focused unit tests for project/group ownership filters, directory expansion, stable hashes, unsafe-path exclusion, symlink escape, non-UTF-8, per-file size, and aggregate limits. Add a regression proving an explicitly shared `.ai-factory` Markdown file remains locally indexable but is absent from cloud snapshots.

  Files: `Cargo.toml`, `Cargo.lock`, `src/cloud/snapshot.rs`, `src/cloud/models.rs`, `src/db/crud.rs`, `src/db/mod.rs`, `src/indexer.rs`, `src/walk.rs`.

  Logging requirements:
  - `INFO`: snapshot start/completion with project slug and category/document counts.
  - `DEBUG`: deterministic skip counts and payload bytes, without content or absolute paths.
  - `WARN`: unsafe, invalid, oversized, or non-UTF-8 document skips and aggregate-limit rejection; identify only safe relative paths when useful.
  - `ERROR`: database/filesystem failures with operation context, never file bodies.

  Dependencies: Task 1.

- [x] Task 3: Add local revision state and the authenticated snapshot HTTP client.

  Deliverable and expected behavior:
  - Add SQLite schema migration v7 for `cloud_sync_state(endpoint, workspace_slug, project_slug, revision, snapshot_hash, updated_at)` plus narrow CRUD methods. Preserve all existing SQLite tables and FTS behavior.
  - Create `src/cloud/client.rs` with bounded connect/request/response timeouts, HTTPS enforcement except loopback test URLs, bearer authentication, request/response size limits, and sanitized status handling. Pass a validated client configuration into this module; it must not read process environment variables itself.
  - Add one direct `reqwest` dependency with blocking, JSON, and rustls features. Use its blocking client here and reuse its async client for JWKS retrieval in Task 5; do not add `ureq` or a second HTTP stack.
  - Leave configuration-source precedence to the Task 8 CLI boundary. The client receives URL, workspace, and token values explicitly; the token is never accepted as a Clap argument, persisted, formatted with `Debug`, or included in errors.
  - Send the stored `base_revision`, snapshot hash/idempotency key, and explicit force flag. Update local revision state only after a successful/no-op response. Keep state unchanged on network, auth, validation, or conflict failures.
  - Map `401`, `403`, `409`, `413`, `429`, and `5xx` to actionable errors. A `409` reports the remote revision/hash and recommends deliberate `--force`; it never retries with overwrite automatically.
  - Add unit and loopback HTTP tests for successful/repeated pushes, missing configuration, HTTPS rules, timeout/size bounds, safe errors, conflicts, and token non-disclosure.

  Files: `Cargo.toml`, `Cargo.lock`, `src/db/schema.rs`, `src/db/crud.rs`, `src/db/mod.rs`, `src/cloud/client.rs`, `src/cloud/models.rs`.

  Logging requirements:
  - `INFO`: push start/completion with endpoint host, workspace/project slug, revision, counts, and duration.
  - `DEBUG`: payload size, snapshot hash prefix, response status, and state transition; never full URLs with query data, headers, bodies, or tokens.
  - `WARN`: `409`, throttling, and recoverable server conditions.
  - `ERROR`: sanitized transport/protocol errors with status and bounded non-sensitive message.

  Dependencies: Tasks 1-2.

<!-- Commit checkpoint: tasks 1-3 -->

### Phase 2: PostgreSQL Cloud Mirror Service

- [x] Task 4: Create the PostgreSQL JSONB/document read model with enforced tenant isolation.

  Deliverable and expected behavior:
  - Add `migrations/0001_cloud_read_model.sql` defining `cloud_workspaces`, `cloud_project_snapshots`, and `cloud_documents`, including primary/unique keys, revision/hash/audit columns, generated `tsvector`, and required indexes.
  - Add `tokio`, `sqlx` with PostgreSQL/migrations/JSON/UUID/rustls runtime features, and `uuid` with serde support. Do not add an ORM or a second connection pool.
  - Enable and force RLS on every tenant table. Policies use transaction-local `app.workspace_id`; the service database role must not own or bypass RLS.
  - Separate schema ownership from runtime access: migrations run out of process with an administrator/owner connection, while the hosted process receives only a non-owner, non-superuser, non-`BYPASSRLS` runtime connection and never runs migrations. Store integration tests execute tenant queries through that runtime role and assert its role flags and lack of tenant-table ownership.
  - Create `src/cloud/store.rs` with a SQLx pool and transactions that set tenant context before every tenant query. Keep all PostgreSQL SQL in this module; local `db::Db` remains SQLite-only.
  - Implement snapshot status/read, atomic project-partition replacement, identical-hash no-op, revision comparison, document projection replacement, indexed `(workspace_id, document_key)` reads, workspace context reads, note/file search, service graph assembly, and event-key lookup from the owning project snapshot.
  - Use explicit `'simple'::regconfig` for both generated `tsvector` values and `websearch_to_tsquery`, PostgreSQL ranking for user text, and deterministic tie-breaking. Do not depend on the database's default text-search configuration or attempt to reproduce SQLite FTS5 rank values.
  - Add store tests for create/replace/delete-within-partition, idempotency, conflict, language-neutral FTS, and cross-workspace denial through the restricted runtime role.

  Files: `Cargo.toml`, `Cargo.lock`, `migrations/0001_cloud_read_model.sql`, `src/cloud/store.rs`, `src/cloud/models.rs`.

  Logging requirements:
  - `INFO`: pool/service readiness and committed snapshot revision/counts.
  - `DEBUG`: query category, workspace/project identifiers, duration, affected row counts, and no-op detection; never raw JSON/content.
  - `WARN`: revision conflict and RLS/authorization denial.
  - `ERROR`: migration, pool, transaction, and query failures with SQL operation names but no bound sensitive values.

  Dependencies: Task 1.

- [x] Task 5: Add the hosted HTTP shell and external OAuth/OIDC resource-server boundary.

  Deliverable and expected behavior:
  - Create `src/cloud/auth.rs` and `src/cloud/http.rs` with validated configuration for bind address, canonical public MCP URI, issuer, audience, JWKS URI, and PostgreSQL URL.
  - Add `axum` and `jsonwebtoken`; reuse the Tokio, UUID, and async `reqwest` dependencies introduced by Tasks 3-4. Add no embedded OAuth server, SSE framework, CORS layer, or in-process TLS dependency.
  - Validate bearer JWT signature/key ID, exact issuer/audience, expiration/not-before, subject, `workspace_id`, `workspace_slug`, and required scope. Cache JWKS with bounded refresh and refresh safely on unknown key ID; fail closed when validation is unavailable.
  - Publish RFC 9728 Protected Resource Metadata and return compliant `WWW-Authenticate` challenges. Implement only the resource server; do not issue tokens or store refresh tokens.
  - Add `/healthz` and database-backed `/readyz`. Apply request body/time limits and safe request IDs. Validate `Origin` on MCP requests when present.
  - Ensure every tenant handler opens a transaction, sets `app.workspace_id`, performs its work, and closes the transaction before returning the pooled connection.
  - Add auth/config/health tests using fixture keys/tokens; no network-dependent tests.

  Files: `Cargo.toml`, `Cargo.lock`, `src/cloud/auth.rs`, `src/cloud/http.rs`, `src/cloud/mod.rs`, `src/cloud/store.rs`.

  Logging requirements:
  - `INFO`: server bind/public URI, readiness transitions, and authenticated request method/path/status/duration.
  - `DEBUG`: request ID, subject identifier, workspace ID, scopes, JWT key ID, and JWKS refresh outcome; hash/redact subject if current logging policy requires it.
  - `WARN`: missing/invalid/expired token, insufficient scope, invalid Origin, and readiness failure.
  - `ERROR`: JWKS/config/database failures. Never log bearer tokens, JWT payloads, Authorization headers, or response content.

  Dependencies: Tasks 1, 3, and 4.

- [x] Task 6: Implement the authenticated atomic snapshot push endpoint.

  Deliverable and expected behavior:
  - Add `PUT /api/v1/workspaces/{workspace_slug}/projects/{project_slug}/snapshot` requiring `ai-workspace:push`.
  - Cross-check token workspace claims with the route and payload; project slug/name and all relative paths must pass server-side validation even if the client already validated them.
  - On the first authorized push, create `cloud_workspaces` from the validated token's workspace UUID/slug. Thereafter reject any UUID/slug mismatch; force mode never bypasses this binding or RLS.
  - Enforce the 16 MiB canonical snapshot limit, 1,000-document, per-document 1 MiB, UTF-8, schema-version, and content-kind limits before persistence. Define the HTTP request-body limit separately as the snapshot limit plus a small bounded envelope allowance so every valid snapshot can be transported without weakening the snapshot ceiling.
  - Recompute the canonical snapshot SHA-256 after server-side validation and reject a supplied hash that does not match. Use only the recomputed hash for identical-content no-op detection, revision responses, and persistence.
  - In one PostgreSQL transaction, acquire a transaction-scoped advisory lock derived from `(workspace_id, project_slug)` before checking whether the project row exists, then lock/read the row, perform identical-hash no-op detection, compare `base_revision` unless explicitly forced, replace the JSONB snapshot and document projection, and return revision/hash/counts. This must serialize concurrent first pushes as well as later revisions.
  - Return `409` with current revision/hash for stale writes, without content. A forced overwrite still records subject, previous revision/hash, and new revision/hash in safe audit fields/logs.
  - Add handler/store integration tests for first push, simultaneous first pushes, repeated retry after lost response, replacement deletion, concurrent revisions, forced overwrite, hash mismatch, invalid tenant/slug/schema/content, and snapshot/request boundary sizes.

  Files: `src/cloud/http.rs`, `src/cloud/store.rs`, `src/cloud/models.rs`.

  Logging requirements:
  - `INFO`: accepted/no-op snapshot with workspace/project, revision, category counts, subject, and duration.
  - `DEBUG`: validated payload bytes/hash prefix and row replacement counts.
  - `WARN`: stale revision, forced overwrite, limit rejection, and claim/route mismatch.
  - `ERROR`: transaction rollback with request/workspace/project context, never snapshot content.

  Dependencies: Tasks 2, 4, and 5.

<!-- Commit checkpoint: tasks 4-6 -->

### Phase 3: Read-only Hosted MCP and CLI Integration

- [x] Task 7: Implement the stateless hosted MCP adapter and positive remote tool allowlist.

  Deliverable and expected behavior:
  - Create `src/cloud/mcp.rs`; expose only common JSON-RPC envelopes from `src/mcp/protocol.rs` as `pub(crate)` rather than routing hosted calls through local SQLite handlers.
  - Implement MCP `2026-07-28` `POST /mcp`, `server/discover`, `tools/list`, and `tools/call`. Every request must carry `params._meta["io.modelcontextprotocol/protocolVersion"] == "2026-07-28"`; validate supported client metadata when present, require `MCP-Protocol-Version` and `Mcp-Method` to match the body, and require matching `Mcp-Name` for `tools/call` only. Reject missing, unsupported, or mismatched modern envelopes with the specified HTTP/JSON-RPC error.
  - Return `server/discover` with `supportedVersions`, capabilities, instructions, `ttlMs`, `cacheScope`, and `_meta["io.modelcontextprotocol/serverInfo"]`; stamp server identity in response metadata as required by the modern protocol. Return deterministic `tools/list` ordering and explicit `ttlMs`/`cacheScope`, and keep every `tools/call` result in the standard `CallToolResult` shape with a required `content` array.
  - Require `ai-workspace:read` and use token-derived workspace context for every call.
  - Implement the seven allowed tools against `cloud::store`: context, synced document read, note search, Markdown FTS, service graph, events, and event details. Use cloud slugs/document keys, never local IDs/paths.
  - Apply the Cloud object lookup contract exactly: searches return indexed `document_key`; reads consume that key; event lists return `event_key`; event details derive/load the owning project snapshot and match the exact key.
  - Direct calls to every local-only/unknown tool must fail even when omitted from discovery. Do not add SSE, session storage, hosted `initialize`, filesystem access, remote writes, CodeGraph, tree, or grep.
  - Add protocol/catalog/handler tests for missing or mismatched body metadata and headers, unsupported versions, applicable `Mcp-Name` rules, discovery/server identity, invalid Origin/content type, read scope, cross-tenant isolation, standard `CallToolResult` shapes, cache metadata, and direct unsafe-tool rejection.

  Files: `src/cloud/mcp.rs`, `src/cloud/http.rs`, `src/cloud/store.rs`, `src/mcp/mod.rs`, `src/mcp/protocol.rs`.

  Logging requirements:
  - `INFO`: MCP request ID, method/tool name, workspace, status, result count, and duration.
  - `DEBUG`: protocol version/header validation and query category, never tool arguments containing queries/content.
  - `WARN`: unsupported protocol, header/body mismatch, invalid Origin, insufficient scope, and unsafe/unknown tool calls.
  - `ERROR`: internal/store errors with request ID only; do not reuse local raw request/argument logging.

  Dependencies: Tasks 4-6.

- [x] Task 8: Wire `cloud push` and `cloud serve` into the existing CLI without changing offline paths.

  Deliverable and expected behavior:
  - Add nested `CloudCommand::{Push, Serve}` in `src/cli/mod.rs` following existing nested subcommand patterns.
  - `cloud push` resolves the current project through `require_project`, accepts `--include-markdown`, `--force`, `--url`, and `--workspace`; resolves URL/workspace as flag then `AI_WORKSPACE_CLOUD_URL`/`AI_WORKSPACE_CLOUD_WORKSPACE`, obtains the token only from `AI_WORKSPACE_CLOUD_TOKEN`, builds the snapshot, pushes it, saves accepted revision state, and prints the existing success/info style.
  - Dispatch cloud commands before `normalize_config_override`. `cloud serve` starts the Tokio runtime and hosted server only for that subcommand and must not read `AI_WORKSPACE_CONFIG` or open the default SQLite database.
  - Preserve `ai-workspace sync`, `export`, `serve`, all env-based local MCP scopes, and every no-cloud command. With no cloud command, the binary performs no cloud configuration lookup or network call.
  - Add parser/dispatch tests and offline regressions. Confirm `Debug` output of parsed commands cannot contain the token because no token field exists.

  Files: `src/cli/mod.rs`, `src/main.rs`, `src/cloud/mod.rs`, `src/cloud/client.rs`, `src/cloud/http.rs`.

  Logging requirements:
  - `INFO`: selected cloud operation and safe workspace/project/host metadata.
  - `DEBUG`: resolved non-secret configuration source, counts, revision, and duration.
  - `WARN`: explicit force use and optional Markdown skip summary.
  - `ERROR`: actionable sanitized command failure; never log tokens or content.

  Dependencies: Tasks 3 and 5-7.

- [x] Task 9: Add end-to-end cloud security, synchronization, and compatibility coverage in CI.

  Deliverable and expected behavior:
  - Create `tests/cloud_push.rs`, `tests/cloud_server.rs`, and `tests/mcp_http_tests.rs` using temp SQLite/project fixtures, local HTTP servers, fixture JWT/JWKS data, and a disposable PostgreSQL database.
  - Verify metadata-only and opted-in Markdown snapshots, project-owned filtering, safe file exclusions, deterministic retries, local state updates, `409`/`--force`, atomic replacement deletion, PostgreSQL FTS, token/scope failures, RLS cross-tenant denial, hosted MCP allowlist, and unchanged local stdio MCP.
  - Assert no absolute path, local ID, token, sensitive filename/content, or raw request body appears in cloud payloads or captured logs.
  - Update `.github/workflows/test.yml` with a dedicated Linux PostgreSQL service job. Its administrator connection applies migrations and creates a separate non-owner, non-superuser, non-`BYPASSRLS` runtime role; all store/server assertions use the runtime connection. Default cross-platform `cargo test` must still pass when no external PostgreSQL is configured; database integration cases run only when their explicit administrator and runtime test database URLs are present.
  - Keep existing fmt/clippy/audit/MCP workflows intact.

  Files: `tests/cloud_push.rs`, `tests/cloud_server.rs`, `tests/mcp_http_tests.rs`, `tests/mcp_tests.rs`, `.github/workflows/test.yml`.

  Logging requirements:
  - Tests capture logs only to assert redaction and useful failure context.
  - Test server diagnostics use `DEBUG`; test failures include request IDs/statuses but never tokens or content.
  - Production code remains controlled by `RUST_LOG` with no test-only verbose behavior.

  Dependencies: Tasks 2-8.

<!-- Commit checkpoint: tasks 7-9 -->

### Phase 4: Documentation and Verification

- [x] Task 10: Complete the mandatory cloud documentation and architecture checkpoint.

  Deliverable and expected behavior:
  - Route user-facing documentation through `$aif-docs` as required by `Docs: yes`.
  - Create `docs/cloud.md` covering external OAuth/OIDC requirements, token claims/scopes, PostgreSQL setup/migrations, ingress TLS/rate limits, environment variables, `cloud push`, `cloud serve`, conflict/force behavior, consent and size limits, health/readiness, and the read-only remote tool list.
  - Update `README.md`, `docs/getting-started.md`, `docs/cli.md`, and `docs/mcp-server.md` to distinguish local stdio/full local tools from hosted read-only synchronized context.
  - Update `AGENTS.md` and `.ai-factory/ARCHITECTURE.md` project maps/boundaries for `src/cloud/` and PostgreSQL migrations without changing the existing local layered rules.
  - Explicitly document that v1 has no web UI, Git import, pull/background sync, remote file writes, cloud CodeGraph, object storage, embedded OAuth server, or bundled TLS.

  Files: `docs/cloud.md`, `README.md`, `docs/getting-started.md`, `docs/cli.md`, `docs/mcp-server.md`, `AGENTS.md`, `.ai-factory/ARCHITECTURE.md`.

  Logging requirements:
  - Documentation changes add no runtime logs.
  - Examples must demonstrate `RUST_LOG` safely and must never place real bearer tokens in command-line arguments or committed files.

  Dependencies: Tasks 8-9.

- [x] Task 11: Run the required project gates and hosted/local smoke checks, fixing all failures.

  Deliverable and expected behavior:
  - Run the repository checklist in exact order with the cloud test database configured for `cargo test`: `cargo fmt`, `cargo clippy`, `cargo test`, `cargo audit`.
  - Run a local stdio MCP smoke call and a hosted push→remote-MCP read/search smoke flow through the authenticated HTTP boundary.
  - Verify a no-cloud local invocation works with cloud environment variables absent, an unauthorized hosted request returns the required challenge, a cross-workspace request is denied by RLS, and the remote tool catalog contains no local-only tools.
  - Fix every code/test/docs issue found; do not weaken assertions, skip security checks, or mark `cargo audit` passed when advisory data/network access is unavailable.

  Files: any files already owned by Tasks 1-10 when verification exposes a defect; no standalone report artifact.

  Logging requirements:
  - Use `RUST_LOG=debug` for smoke diagnostics and inspect output for token/content leakage.
  - Preserve concise `INFO` production defaults and environment-controlled verbosity.
  - Record exact failing command/error in the implementation handoff if an external dependency prevents completion.

  Dependencies: Tasks 9-10.

<!-- Commit checkpoint: tasks 10-11 -->

## Acceptance Criteria

- Existing SQLite commands, local FTS/CodeGraph, and stdio MCP work offline with no cloud account or network.
- `cloud push` publishes one current-project snapshot, is deterministic/idempotent, safely mirrors deletions within that project partition, and rejects stale revisions unless the user explicitly supplies `--force`.
- File bodies are absent by default; opt-in Markdown upload never includes hidden, sensitive, escaped, oversized, non-UTF-8, non-Markdown, or unshared content.
- The hosted service stores no local absolute paths or device-local IDs and exposes only synchronized read-only context.
- OAuth/OIDC validation, token scopes, application predicates, and forced PostgreSQL RLS prevent cross-workspace access; secrets/content do not appear in logs.
- Hosted MCP conforms to the selected MCP `2026-07-28` HTTP contract and exposes exactly the seven approved tools. Local MCP remains on its current compatibility contract.
- PostgreSQL search returns note and Markdown matches from the latest accepted project snapshots.
- Documentation describes a deployable reverse-proxy/PostgreSQL/OIDC setup and clearly states all deferred capabilities.
- `cargo fmt`, `cargo clippy`, `cargo test`, and `cargo audit` complete in the mandated order.

## Risks and Deferred Work

- The first release is push-only and current-project-owned. General bidirectional synchronization, pull, background daemon/watchers, CRDT/merge logic, device IDs, and tombstones are deferred until real multi-writer demand exists.
- Git-backed import is deferred because it cannot mirror local/group notes and events without another ownership model.
- OAuth token issuance and team administration are delegated to the configured authorization server; this repository only validates tokens and publishes resource metadata.
- Hosted MCP supports only current `2026-07-28` clients. Add older hosted protocol compatibility only when a named target client requires it.
- PostgreSQL JSONB snapshot assembly is intentionally simple and may load multiple project snapshots per workspace. Normalize more tables only after measured workspace size/query latency justifies it.
- Source files beyond opted-in safe Markdown, object storage, browser UI, remote writes, cloud CodeGraph, SSE, protocol sessions, in-process TLS, rate limiting, and container/orchestrator manifests are out of scope for v1.
