[← MCP Server](mcp-server.md) · [Back to README](../README.md) · [Contributing →](contributing.md)

# Cloud Synchronization and Hosted MCP

Cloud mode publishes a bounded snapshot of one local project to PostgreSQL and exposes that synchronized data through a read-only HTTP MCP endpoint. Local SQLite, local stdio MCP, and existing offline commands continue to work independently.

## What v1 Includes

- Explicit `ai-workspace cloud push` synchronization
- Project/group metadata, notes, service links, dependencies, and events
- Optional safe Markdown content from explicitly shared scopes
- PostgreSQL JSONB snapshots plus a full-text document projection
- External OAuth 2.0/OpenID Connect (OIDC) bearer-token validation
- Seven read-only hosted MCP tools

Cloud v1 has no web UI, Git import, pull or background sync, remote file writes, cloud CodeGraph, object storage, embedded OAuth server, or bundled TLS.

## Push a Project

Tokens are read only from the environment. Do not place bearer tokens in command arguments or committed files.

```bash
export AI_WORKSPACE_CLOUD_URL=https://workspace.example.com
export AI_WORKSPACE_CLOUD_WORKSPACE=platform
export AI_WORKSPACE_CLOUD_TOKEN="$(security find-generic-password -w -s ai-workspace-cloud)"
ai-workspace cloud push
```

The default snapshot contains metadata and notes. Include Markdown content deliberately:

```bash
ai-workspace cloud push --include-markdown
```

Only UTF-8 `.md` files within explicit shared file/directory scopes are eligible. Hidden paths, credential-like paths, symlink escapes, missing files, non-Markdown files, non-UTF-8 files, and files over 1 MiB are skipped. A snapshot is limited to 1,000 documents and 16 MiB of canonical JSON.

### Snapshot compatibility

`snapshot.schema_version` is mandatory. This server accepts exactly version `1`, independently of the CLI package version and MCP protocol version. Omitted collections default to empty; unknown fields are rejected. Breaking wire changes, including fields that older strict readers reject, require a new schema version. Future servers must explicitly retain a supported decoder for older clients or require a coordinated CLI/server upgrade; there is no implicit conversion or downgrade.

An unsupported numeric schema version returns HTTP `400` with `code: "invalid_snapshot"` and an unsupported-version message. Missing fields or unknown fields fail JSON extraction with `422`. Neither changes stored data. Upgrade to a CLI/server pair supporting the same snapshot version before retrying.

### Revision conflicts

Each accepted push stores the returned revision locally. A later push sends that revision as its optimistic concurrency base. A stale base returns `409` without replacing cloud data.

Inspect the competing change before overwriting it. If replacement is intentional:

```bash
ai-workspace cloud push --force
```

Force mode requires both `ai-workspace:push` and the separate `ai-workspace:push-force` scope, including retries that would be no-ops. Without either permission the server returns `403 insufficient_scope` and advertises the missing scope in `WWW-Authenticate`, before accessing storage. Grant the extra scope only to identities permitted to override conflicts. Force does not bypass authentication, workspace binding, validation, or PostgreSQL row-level security (RLS). The service records the subject and previous revision/hash.

Authorization is enforced by `cloud::http::push_authenticated`, the only production caller of `CloudStore::replace_project_snapshot`. The store's `force: bool` controls concurrency after authorization; it does not grant permission. Any future entry point calling the store directly must enforce the same scope checks first.

The server assigns the next revision under a transaction lock and writes its own database timestamps. Clients cannot supply the stored revision or timestamps. After A is replaced by B, replaying the original A request returns `409` and leaves B and its audit fields unchanged. A retry whose hash already matches the current snapshot is a harmless no-op.

This is optimistic concurrency control. A writer can intentionally restore older content by supplying the current `base_revision`; bypassing a stale base with `force` requires the additional scope. The content hash is an integrity check, not a signature proving freshness. Protect write tokens accordingly.

## Run the Hosted Service

`cloud serve` uses cloud variables only; it does not open the local SQLite database or read `AI_WORKSPACE_CONFIG`.

| Variable | Required | Purpose |
|----------|----------|---------|
| `AI_WORKSPACE_CLOUD_BIND` | No | Listen address; defaults to `127.0.0.1:8080` |
| `AI_WORKSPACE_CLOUD_PUBLIC_MCP_URI` | Yes | Public HTTPS MCP URL, for example `https://workspace.example.com/mcp` |
| `AI_WORKSPACE_CLOUD_DATABASE_URL` | Yes | Restricted PostgreSQL runtime connection URL |
| `AI_WORKSPACE_CLOUD_OIDC_ISSUER` | Yes | Exact trusted token issuer |
| `AI_WORKSPACE_CLOUD_OIDC_AUDIENCE` | Yes | Required token audience |
| `AI_WORKSPACE_CLOUD_OIDC_JWKS_URI` | Yes | HTTPS JSON Web Key Set (JWKS) URL |

```bash
export AI_WORKSPACE_CLOUD_BIND=127.0.0.1:8080
export AI_WORKSPACE_CLOUD_PUBLIC_MCP_URI=https://workspace.example.com/mcp
export AI_WORKSPACE_CLOUD_DATABASE_URL=postgres://cloud_runtime@db/ai_workspace
export AI_WORKSPACE_CLOUD_OIDC_ISSUER=https://identity.example.com
export AI_WORKSPACE_CLOUD_OIDC_AUDIENCE=ai-workspace
export AI_WORKSPACE_CLOUD_OIDC_JWKS_URI=https://identity.example.com/.well-known/jwks.json
RUST_LOG=info ai-workspace cloud serve
```

HTTP JWKS URLs are accepted only on loopback for tests. Production public and JWKS URLs must use HTTPS.

## OAuth/OIDC Contract

The service is an OAuth resource server; it does not issue or refresh tokens. A trusted JWT must have a supported RSA signature and these claims:

```json
{
  "iss": "https://identity.example.com",
  "aud": "ai-workspace",
  "sub": "user-or-service-id",
  "workspace_id": "018f4f84-f424-7e31-9d29-3f6ad8b33980",
  "workspace_slug": "platform",
  "scope": "ai-workspace:read ai-workspace:push",
  "exp": 1924992000
}
```

Use `ai-workspace:push` for the snapshot endpoint and `ai-workspace:read` for hosted MCP. For `--force`, request `ai-workspace:push-force` in addition to `ai-workspace:push`; it does not imply ordinary push access. Existing push tokens must gain the extra scope before they can override conflicts. All three scopes are advertised in the protected-resource metadata. The workspace UUID and slug in the token are bound on first push and checked on every request.

### JWKS lifecycle and revocation

- Signing keys are cached for 300 seconds per server process. An unknown `kid` or expired cache triggers a refresh. Refreshes are serialized and start at most once every five seconds, including failed attempts, to bound random-key traffic. A newly rotated key may therefore require a retry after five seconds.
- A successful refresh replaces the complete key set, including removal of retired keys. Publish new keys before issuing tokens with them and overlap old keys while their tokens remain valid.
- JWKS HTTP requests time out after ten seconds and are limited to 256 KiB while reading. During an identity-provider outage, known keys in a fresh cache continue to work. Unknown keys and expired caches fail closed with `401`; failed refreshes never extend cache validity. An old cache may remain allocated after an error, but every key lookup checks its original load time: after 300 seconds even a known `kid` is rejected, including during the refresh cooldown. There is no stale-key fallback.
- JWT signature, issuer, audience, expiration and optional `nbf` are checked on every request, with 60 seconds of clock leeway. Offline JWT validation does not detect individual token revocation or logout before expiration. Issue short-lived access tokens. Deploy gateway introspection or a revocation policy if immediate revocation is required; neither is built into v1. Removing a signing key takes effect on refresh, and cached keys expire after five minutes.

## PostgreSQL Setup

Apply [the cloud migration](../migrations/0001_cloud_read_model.sql) as an administrator or migration owner:

```bash
psql "$AI_WORKSPACE_CLOUD_ADMIN_DATABASE_URL" -v ON_ERROR_STOP=1 -f migrations/0001_cloud_read_model.sql
```

Create a separate runtime role. It must not be a superuser, have `BYPASSRLS`, or own the tenant tables:

```sql
CREATE ROLE cloud_runtime LOGIN PASSWORD 'replace-through-your-secret-manager'
  NOSUPERUSER NOBYPASSRLS;
GRANT USAGE ON SCHEMA public TO cloud_runtime;
GRANT SELECT, INSERT, UPDATE, DELETE
  ON cloud_workspaces, cloud_project_snapshots, cloud_documents
  TO cloud_runtime;
```

The service refuses to start with an unsafe runtime role. Every tenant transaction sets a transaction-local workspace context, and all three cloud tables have forced RLS policies.

The store revalidates each snapshot's schema, contents and canonical serialized size, and verifies its hash, before opening a write transaction. Direct internal callers therefore cannot bypass the HTTP/builder checks. Snapshot replacement uses a 64-bit advisory-lock hash of workspace UUID and project slug. A hash collision can make unrelated pushes wait or hit the statement timeout; it cannot select another project's data or bypass RLS because SQL predicates still use the full identifiers. This contention risk is accepted; splitting the key into two 32-bit hashes would not increase the total key space.

## Hosted MCP

Send JSON-RPC requests to `POST /mcp` with:

- `Authorization: Bearer ...`
- `Content-Type: application/json`
- `MCP-Protocol-Version: 2026-07-28`
- `Mcp-Method` matching the JSON-RPC method
- `Mcp-Name` matching the tool name for `tools/call`
- `params._meta["io.modelcontextprotocol/protocolVersion"]` set to `2026-07-28`

Supported methods are `server/discover`, `tools/list`, and `tools/call`. The hosted catalog is intentionally limited:

| Tool | Purpose |
|------|---------|
| `workspace_context` | Read synchronized project snapshots |
| `workspace_read` | Read one indexed `document_key` |
| `workspace_search` | Search synchronized notes |
| `workspace_search_fulltext` | Search synchronized Markdown |
| `workspace_service_graph` | Read synchronized service links |
| `workspace_events` | List synchronized events |
| `workspace_event_details` | Read one exact `event_key` |

Hosted calls cannot access local paths, run tree/grep, write files, or query CodeGraph. See [MCP Server](mcp-server.md) for the separate local stdio tool surface.

### Request and query limits

| Boundary | Behavior |
|----------|----------|
| MCP request body | 64 KiB maximum; larger requests return `413` |
| Snapshot request body | 16 MiB + 64 KiB envelope allowance; canonical snapshot remains limited to 16 MiB |
| HTTP handler | 30-second outer timeout (`408`); MCP dispatch also has a 20-second deadline |
| PostgreSQL | Pool of ten connections, five-second acquisition timeout, ten-second statement timeout in tenant transactions |
| Search queries | 1,024 UTF-8 bytes maximum; excessive queries return `400` / JSON-RPC `-32602`. Exact keys remain subject to the request-body limit |
| Search results | Default 20, clamped to 1–100; indexed PostgreSQL full-text search, no regex or filesystem scanning |
| Workspace collection reads | At most 100 projects and 4 MiB of PostgreSQL snapshot JSON text, checked in one SQL statement before returning payloads to the application. Applies to context, service graph and event lists; over-limit workspaces return `422` / JSON-RPC `-32602` |
| MCP success response | 8 MiB including JSON escaping, metadata and both content representations; excessive results return `422` / JSON-RPC `-32602` with guidance to use search or exact keys |

V1 does not support cursors or offset pagination. Searches return only their bounded top results; context, service graph and event lists are complete or fail the storage budget, never silently truncated. The SQL query examines at most 101 ordered project rows and suppresses snapshot payloads when either budget is exceeded. Deserialized values and MCP's two content representations still add bounded application overhead, and the final 8 MiB response cap remains in force. Use narrower search terms and exact document/event keys for over-limit workspaces. Keep ingress concurrency and deployment memory bounded; add pagination when larger complete collection reads are needed.

Rate limiting belongs to the ingress and is not enforced by the binary. Configure both unauthenticated source-IP limits and authenticated tenant/subject limits across all replicas, return `429` with `Retry-After`, and budget concurrent requests separately. Derive identity from verified claims, never arbitrary request headers. Choose quotas for the deployment's workload rather than treating the HTTP timeout as a rate limit.

### Audit records

At `RUST_LOG=info`, completed authenticated MCP and snapshot requests emit a `cloud audit` JSON record with UTC Unix milliseconds, a SHA-256 subject identifier, workspace UUID, scope, operation, HTTP result and duration. MCP records include the allowlisted method/tool and a hashed document/event target when applicable; pushes include a hashed project slug. Unknown method/tool input is replaced by a fixed label. Queries, raw resource keys, request IDs, tokens and content are not included in these audit records.

The subject hash is a stable correlation identifier, not anonymization or an authorization credential. Operators can correlate it with the issuer's subject; protect and retain these logs according to deployment policy. Authentication and envelope failures have diagnostic logs rather than a trusted subject. Ingress access logs must cover malformed JSON/body rejections before the handlers, disconnected requests and requests cancelled by the outer timeout. Snapshot rows also retain the last push subject, previous revision/hash and force flag; they are not an append-only audit history.

### Extension boundaries

Storage, identity validation, HTTP access checks and hosted tools already live in separate `cloud::store`, `cloud::auth`, `cloud::http` and `cloud::mcp` modules. Keep these concrete boundaries for v1. Add storage/identity/access-policy traits or an event-stream interface when a second implementation or integration has concrete requirements; no unused extension interfaces are introduced here.

## Operations and Security

- Terminate TLS at a reverse proxy or ingress; the binary does not bundle TLS.
- Apply request and per-token rate limits at ingress.
- Keep the service behind bounded connection, body, and request timeouts.
- Expose `GET /healthz` for process health and `GET /readyz` for PostgreSQL readiness.
- Use `GET /.well-known/oauth-protected-resource` for OAuth resource metadata.
- Set `RUST_LOG=info` normally and `RUST_LOG=debug` only while troubleshooting. Logs exclude tokens, snapshot content, tool queries, and raw request bodies.
- Back up PostgreSQL according to your deployment's recovery requirements.

## See Also

- [Getting Started](getting-started.md) — Local installation and project setup
- [CLI Reference](cli.md) — Cloud command flags and local commands
- [MCP Server](mcp-server.md) — Full local stdio MCP reference
