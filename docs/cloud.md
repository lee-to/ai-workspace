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

### Revision conflicts

Each accepted push stores the returned revision locally. A later push sends that revision as its optimistic concurrency base. A stale base returns `409` without replacing cloud data.

Inspect the competing change before overwriting it. If replacement is intentional:

```bash
ai-workspace cloud push --force
```

Force mode does not bypass authentication, workspace binding, validation, or PostgreSQL row-level security (RLS). The service records the subject and previous revision/hash.

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

Use `ai-workspace:push` for the snapshot endpoint and `ai-workspace:read` for hosted MCP. The workspace UUID and slug in the token are bound on first push and checked on every request.

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
