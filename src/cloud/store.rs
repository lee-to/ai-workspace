use super::models::CloudProjectSnapshot;
use anyhow::{Context as _, Result, bail};
use log::{debug, error, info, warn};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::time::Instant;
use uuid::Uuid;

pub const MAX_CLOUD_CONTEXT_PROJECTS: i64 = 100;
pub const MAX_CLOUD_CONTEXT_BYTES: i64 = 4 * 1024 * 1024;

#[derive(Debug)]
pub struct ContextLimitExceeded;

impl std::fmt::Display for ContextLimitExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Cloud workspace context exceeds {MAX_CLOUD_CONTEXT_PROJECTS} projects or {MAX_CLOUD_CONTEXT_BYTES} bytes"
        )
    }
}

impl std::error::Error for ContextLimitExceeded {}

#[derive(Clone)]
pub struct CloudStore {
    pool: PgPool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotStatus {
    pub revision: i64,
    pub snapshot_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplaceSnapshotOutcome {
    Accepted {
        revision: i64,
        snapshot_hash: String,
        no_op: bool,
    },
    Conflict(SnapshotStatus),
}

#[derive(Debug, Clone, Serialize)]
pub struct CloudSearchHit {
    pub document_key: String,
    pub project_slug: String,
    pub kind: String,
    pub scope: Option<String>,
    pub group_slug: Option<String>,
    pub relative_path: Option<String>,
    pub label: Option<String>,
    pub snippet: String,
    pub rank: f32,
}

impl CloudStore {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let started = Instant::now();
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(database_url)
            .await
            .map_err(|error| {
                error!("cloud store pool connection failed: {error}");
                error
            })
            .context("Failed to connect to cloud PostgreSQL")?;
        let store = Self { pool };
        store.verify_runtime_role().await?;
        info!(
            "cloud store ready duration_ms={}",
            started.elapsed().as_millis()
        );
        Ok(store)
    }

    pub async fn ready(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .context("Cloud PostgreSQL readiness query failed")?;
        Ok(())
    }

    async fn verify_runtime_role(&self) -> Result<()> {
        let row = sqlx::query(
            "SELECT r.rolsuper, r.rolbypassrls,
                    EXISTS (
                        SELECT 1
                        FROM pg_class c
                        JOIN pg_namespace n ON n.oid = c.relnamespace
                        WHERE n.nspname = current_schema()
                          AND c.relname IN (
                              'cloud_workspaces',
                              'cloud_project_snapshots',
                              'cloud_documents'
                          )
                          AND c.relowner = r.oid
                    ) AS owns_tenant_table
             FROM pg_roles r
             WHERE r.rolname = current_user",
        )
        .fetch_one(&self.pool)
        .await
        .context("Failed to inspect cloud runtime database role")?;
        let is_superuser: bool = row.try_get("rolsuper")?;
        let bypasses_rls: bool = row.try_get("rolbypassrls")?;
        let owns_tenant_table: bool = row.try_get("owns_tenant_table")?;
        if is_superuser || bypasses_rls || owns_tenant_table {
            bail!(
                "Cloud runtime database role must be non-superuser, non-BYPASSRLS, and must not own tenant tables"
            );
        }
        Ok(())
    }

    async fn begin_tenant(&self, workspace_id: Uuid) -> Result<Transaction<'_, Postgres>> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("SELECT set_config('app.workspace_id', $1, true), set_config('statement_timeout', '10s', true)")
            .bind(workspace_id.to_string())
            .execute(&mut *transaction)
            .await
            .context("Failed to set PostgreSQL tenant context")?;
        Ok(transaction)
    }

    /// Persist an already-authorized push. The HTTP caller (`push_authenticated`)
    /// requires `ai-workspace:push` and, for `force`, `ai-workspace:push-force`
    /// before calling this method, including for no-op retries. Internal callers
    /// must enforce the same policy; `force` is not an authorization credential.
    #[allow(clippy::too_many_arguments)]
    pub async fn replace_project_snapshot(
        &self,
        workspace_id: Uuid,
        workspace_slug: &str,
        snapshot: &CloudProjectSnapshot,
        snapshot_hash: &str,
        base_revision: Option<i64>,
        force: bool,
        subject: &str,
    ) -> Result<ReplaceSnapshotOutcome> {
        let started = Instant::now();
        snapshot.validate()?;
        let snapshot_json = serde_json::to_string(snapshot)?;
        if super::snapshot::sha256_hex(snapshot_json.as_bytes()) != snapshot_hash {
            bail!("Snapshot hash does not match payload");
        }
        let project_slug = &snapshot.project.slug;
        let mut transaction = self.begin_tenant(workspace_id).await?;
        // Hash collisions only serialize unrelated projects; tenant and row checks remain exact.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("{workspace_id}:{project_slug}"))
            .execute(&mut *transaction)
            .await
            .context("Failed to acquire cloud snapshot advisory lock")?;

        sqlx::query(
            "INSERT INTO cloud_workspaces (workspace_id, workspace_slug)
             VALUES ($1::uuid, $2)
             ON CONFLICT (workspace_id) DO NOTHING",
        )
        .bind(workspace_id.to_string())
        .bind(workspace_slug)
        .execute(&mut *transaction)
        .await
        .context("Failed to create cloud workspace binding")?;
        let bound_slug: Option<String> = sqlx::query_scalar(
            "SELECT workspace_slug
             FROM cloud_workspaces
             WHERE workspace_id = $1::uuid",
        )
        .bind(workspace_id.to_string())
        .fetch_optional(&mut *transaction)
        .await?;
        if bound_slug.as_deref() != Some(workspace_slug) {
            warn!(
                "cloud workspace binding denied workspace_id={} requested_slug='{}'",
                workspace_id, workspace_slug
            );
            bail!("Workspace UUID is already bound to a different slug");
        }

        let current = sqlx::query(
            "SELECT revision, snapshot_hash
             FROM cloud_project_snapshots
             WHERE workspace_id = $1::uuid AND project_slug = $2
             FOR UPDATE",
        )
        .bind(workspace_id.to_string())
        .bind(project_slug)
        .fetch_optional(&mut *transaction)
        .await
        .context("Failed to lock cloud project snapshot")?
        .map(|row| SnapshotStatus {
            revision: row.get("revision"),
            snapshot_hash: row.get("snapshot_hash"),
        });

        if current
            .as_ref()
            .is_some_and(|status| status.snapshot_hash == snapshot_hash)
        {
            let current = current.expect("checked above");
            transaction.commit().await?;
            debug!(
                "cloud store snapshot no-op workspace_id={} project_slug='{}' revision={} duration_ms={}",
                workspace_id,
                project_slug,
                current.revision,
                started.elapsed().as_millis()
            );
            return Ok(ReplaceSnapshotOutcome::Accepted {
                revision: current.revision,
                snapshot_hash: current.snapshot_hash,
                no_op: true,
            });
        }
        let revision_matches = match (&current, base_revision) {
            (None, None) => true,
            (Some(current), Some(base)) => current.revision == base,
            _ => false,
        };
        if !force && !revision_matches {
            let conflict = current.unwrap_or(SnapshotStatus {
                revision: 0,
                snapshot_hash: String::new(),
            });
            warn!(
                "cloud store revision conflict workspace_id={} project_slug='{}' current_revision={} base_revision={:?}",
                workspace_id, project_slug, conflict.revision, base_revision
            );
            transaction.rollback().await?;
            return Ok(ReplaceSnapshotOutcome::Conflict(conflict));
        }

        let revision = current.as_ref().map_or(1, |status| status.revision + 1);
        sqlx::query(
            "INSERT INTO cloud_project_snapshots (
                workspace_id, project_slug, project_name, snapshot, revision,
                snapshot_hash, pushed_by, previous_revision, previous_snapshot_hash, forced
             ) VALUES ($1::uuid, $2, $3, $4::jsonb, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (workspace_id, project_slug) DO UPDATE SET
                project_name = excluded.project_name,
                snapshot = excluded.snapshot,
                revision = excluded.revision,
                snapshot_hash = excluded.snapshot_hash,
                pushed_by = excluded.pushed_by,
                previous_revision = excluded.previous_revision,
                previous_snapshot_hash = excluded.previous_snapshot_hash,
                forced = excluded.forced,
                updated_at = now()",
        )
        .bind(workspace_id.to_string())
        .bind(project_slug)
        .bind(&snapshot.project.name)
        .bind(snapshot_json)
        .bind(revision)
        .bind(snapshot_hash)
        .bind(subject)
        .bind(current.as_ref().map(|status| status.revision))
        .bind(current.as_ref().map(|status| status.snapshot_hash.as_str()))
        .bind(force)
        .execute(&mut *transaction)
        .await
        .context("Failed to replace cloud project snapshot")?;
        sqlx::query(
            "DELETE FROM cloud_documents
             WHERE workspace_id = $1::uuid AND project_slug = $2",
        )
        .bind(workspace_id.to_string())
        .bind(project_slug)
        .execute(&mut *transaction)
        .await?;
        for note in &snapshot.notes {
            sqlx::query(
                "INSERT INTO cloud_documents (
                    workspace_id, document_key, project_slug, kind, scope,
                    group_slug, label, content
                 ) VALUES ($1::uuid, $2, $3, 'note', $4, $5, $6, $7)",
            )
            .bind(workspace_id.to_string())
            .bind(&note.cloud_key)
            .bind(project_slug)
            .bind(note.scope.as_str())
            .bind(&note.group_slug)
            .bind(&note.label)
            .bind(&note.content)
            .execute(&mut *transaction)
            .await?;
        }
        for document in &snapshot.documents {
            sqlx::query(
                "INSERT INTO cloud_documents (
                    workspace_id, document_key, project_slug, kind,
                    relative_path, label, content
                 ) VALUES ($1::uuid, $2, $3, 'markdown', $4, $5, $6)",
            )
            .bind(workspace_id.to_string())
            .bind(&document.cloud_key)
            .bind(project_slug)
            .bind(&document.relative_path)
            .bind(&document.label)
            .bind(&document.content)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await.map_err(|error| {
            error!(
                "cloud store transaction commit failed operation='replace_snapshot' workspace_id={} project_slug='{}': {}",
                workspace_id, project_slug, error
            );
            error
        })?;
        info!(
            "cloud store snapshot committed workspace_id={} project_slug='{}' revision={} documents={} notes={} duration_ms={}",
            workspace_id,
            project_slug,
            revision,
            snapshot.documents.len(),
            snapshot.notes.len(),
            started.elapsed().as_millis()
        );
        Ok(ReplaceSnapshotOutcome::Accepted {
            revision,
            snapshot_hash: snapshot_hash.to_owned(),
            no_op: false,
        })
    }

    pub async fn read_document(
        &self,
        workspace_id: Uuid,
        document_key: &str,
    ) -> Result<Option<Value>> {
        let mut transaction = self.begin_tenant(workspace_id).await?;
        let value: Option<String> = sqlx::query_scalar(
            "SELECT jsonb_build_object(
                'document_key', document_key,
                'project_slug', project_slug,
                'kind', kind,
                'scope', scope,
                'group_slug', group_slug,
                'relative_path', relative_path,
                'label', label,
                'content', content
             )::text
             FROM cloud_documents
             WHERE workspace_id = $1::uuid AND document_key = $2",
        )
        .bind(workspace_id.to_string())
        .bind(document_key)
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;
        value
            .map(|value| serde_json::from_str(&value).context("Invalid stored cloud document"))
            .transpose()
    }

    pub async fn workspace_context(&self, workspace_id: Uuid) -> Result<Vec<Value>> {
        let mut transaction = self.begin_tenant(workspace_id).await?;
        let values: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT CASE WHEN count(*) OVER () <= $2
                         AND sum(octet_length(snapshot::text)) OVER () <= $3
                    THEN snapshot::text END
             FROM (
                 SELECT snapshot, project_slug FROM cloud_project_snapshots
                 WHERE workspace_id = $1::uuid
                 ORDER BY project_slug LIMIT $2 + 1
             ) bounded
             ORDER BY project_slug",
        )
        .bind(workspace_id.to_string())
        .bind(MAX_CLOUD_CONTEXT_PROJECTS)
        .bind(MAX_CLOUD_CONTEXT_BYTES)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        if values.iter().any(Option::is_none) {
            return Err(ContextLimitExceeded.into());
        }
        values
            .into_iter()
            .flatten()
            .map(|value| serde_json::from_str(&value).context("Invalid stored cloud snapshot"))
            .collect()
    }

    pub async fn search_documents(
        &self,
        workspace_id: Uuid,
        query: &str,
        kind: Option<&str>,
        limit: i64,
    ) -> Result<Vec<CloudSearchHit>> {
        let started = Instant::now();
        let limit = limit.clamp(1, 100);
        let mut transaction = self.begin_tenant(workspace_id).await?;
        let rows = sqlx::query(
            "SELECT document_key, project_slug, kind, scope, group_slug,
                    relative_path, label,
                    ts_headline('simple'::regconfig, content,
                        websearch_to_tsquery('simple'::regconfig, $2),
                        'MaxWords=30, MinWords=10') AS snippet,
                    ts_rank(search_vector,
                        websearch_to_tsquery('simple'::regconfig, $2)) AS rank
             FROM cloud_documents
             WHERE workspace_id = $1::uuid
               AND ($3::text IS NULL OR kind = $3)
               AND search_vector @@ websearch_to_tsquery('simple'::regconfig, $2)
             ORDER BY rank DESC, project_slug, document_key
             LIMIT $4",
        )
        .bind(workspace_id.to_string())
        .bind(query)
        .bind(kind)
        .bind(limit)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        debug!(
            "cloud store query category='document_search' workspace_id={} kind={:?} results={} duration_ms={}",
            workspace_id,
            kind,
            rows.len(),
            started.elapsed().as_millis()
        );
        rows.into_iter()
            .map(|row| {
                Ok(CloudSearchHit {
                    document_key: row.try_get("document_key")?,
                    project_slug: row.try_get("project_slug")?,
                    kind: row.try_get("kind")?,
                    scope: row.try_get("scope")?,
                    group_slug: row.try_get("group_slug")?,
                    relative_path: row.try_get("relative_path")?,
                    label: row.try_get("label")?,
                    snippet: row.try_get("snippet")?,
                    rank: row.try_get("rank")?,
                })
            })
            .collect()
    }

    pub async fn service_graph(&self, workspace_id: Uuid) -> Result<Vec<Value>> {
        let snapshots = self.workspace_context(workspace_id).await?;
        let mut links = snapshots
            .iter()
            .flat_map(|snapshot| {
                snapshot
                    .get("service_links")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .cloned()
            })
            .collect::<Vec<_>>();
        links.sort_by_key(|link| {
            link.get("cloud_key")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        });
        Ok(links)
    }

    pub async fn events(&self, workspace_id: Uuid) -> Result<Vec<Value>> {
        let snapshots = self.workspace_context(workspace_id).await?;
        let mut events = snapshots
            .iter()
            .flat_map(|snapshot| {
                snapshot
                    .get("events")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .cloned()
            })
            .collect::<Vec<_>>();
        events.sort_by(|left, right| {
            right["created_at"]
                .as_str()
                .cmp(&left["created_at"].as_str())
                .then_with(|| left["cloud_key"].as_str().cmp(&right["cloud_key"].as_str()))
        });
        Ok(events)
    }

    pub async fn event_details(
        &self,
        workspace_id: Uuid,
        event_key: &str,
    ) -> Result<Option<Value>> {
        let source_slug = event_key
            .strip_prefix("event:")
            .and_then(|rest| rest.split(':').next())
            .ok_or_else(|| anyhow::anyhow!("Invalid event key"))?;
        let mut transaction = self.begin_tenant(workspace_id).await?;
        let event: Option<String> = sqlx::query_scalar(
            "SELECT event::text
             FROM cloud_project_snapshots snapshot,
                  LATERAL jsonb_array_elements(snapshot.snapshot->'events') event
             WHERE workspace_id = $1::uuid
               AND project_slug = $2
               AND event->>'cloud_key' = $3",
        )
        .bind(workspace_id.to_string())
        .bind(source_slug)
        .bind(event_key)
        .fetch_optional(&mut *transaction)
        .await?;
        transaction.commit().await?;
        event
            .map(|event| serde_json::from_str(&event).context("Invalid stored cloud event"))
            .transpose()
    }
}

pub fn context_response(snapshots: Vec<Value>) -> Value {
    json!({ "projects": snapshots })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::models::{
        CLOUD_SNAPSHOT_SCHEMA_VERSION, CloudNote, CloudNoteScope, CloudProject,
        CloudProjectSnapshot,
    };

    fn snapshot_hash(snapshot: &CloudProjectSnapshot) -> String {
        crate::cloud::snapshot::sha256_hex(&serde_json::to_vec(snapshot).unwrap())
    }

    #[tokio::test]
    async fn store_rejects_invalid_snapshot_before_database_access() {
        let store = CloudStore {
            pool: PgPoolOptions::new()
                .acquire_timeout(std::time::Duration::from_millis(50))
                .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
                .unwrap(),
        };
        let mut snapshot: CloudProjectSnapshot = serde_json::from_value(json!({
            "schema_version": 0,
            "project": {"cloud_key": "project:demo", "name": "Demo", "slug": "demo"}
        }))
        .unwrap();
        let error = store
            .replace_project_snapshot(
                Uuid::new_v4(),
                "team",
                &snapshot,
                &snapshot_hash(&snapshot),
                None,
                false,
                "user",
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Unsupported cloud snapshot schema version")
        );

        snapshot.schema_version = CLOUD_SNAPSHOT_SCHEMA_VERSION;
        let error = store
            .replace_project_snapshot(
                Uuid::new_v4(),
                "team",
                &snapshot,
                &"a".repeat(64),
                None,
                false,
                "user",
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Snapshot hash does not match payload");

        snapshot.project.name = "x".repeat(super::super::models::MAX_CLOUD_SNAPSHOT_BYTES);
        let error = store
            .replace_project_snapshot(
                Uuid::new_v4(),
                "team",
                &snapshot,
                &snapshot_hash(&snapshot),
                None,
                true,
                "user",
            )
            .await
            .unwrap_err();
        assert!(error.to_string().starts_with("Cloud snapshot exceeds"));
    }

    #[test]
    fn migration_forces_rls_and_pins_simple_text_search() {
        let sql = include_str!("../../migrations/0001_cloud_read_model.sql");
        for table in [
            "cloud_workspaces",
            "cloud_project_snapshots",
            "cloud_documents",
        ] {
            assert!(sql.contains(&format!("ALTER TABLE {table} FORCE ROW LEVEL SECURITY")));
        }
        assert!(sql.contains("'simple'::regconfig"));
    }

    #[tokio::test]
    async fn postgres_workspace_context_limits() {
        let Ok(database_url) = std::env::var("AI_WORKSPACE_CLOUD_TEST_DATABASE_URL") else {
            return;
        };
        let store = CloudStore::connect(&database_url).await.unwrap();
        let workspace_id = Uuid::new_v4();
        let mut transaction = store.begin_tenant(workspace_id).await.unwrap();
        sqlx::query(
            "INSERT INTO cloud_workspaces (workspace_id, workspace_slug) VALUES ($1::uuid, $2)",
        )
        .bind(workspace_id.to_string())
        .bind(format!("bounds-{}", workspace_id.simple()))
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO cloud_project_snapshots
                (workspace_id, project_slug, project_name, snapshot, revision, snapshot_hash, pushed_by)
             SELECT $1::uuid, 'p' || n, 'Project',
                jsonb_build_object('schema_version', 1, 'project', jsonb_build_object(
                    'cloud_key', 'project:p' || n, 'slug', 'p' || n, 'name', 'Project'),
                    'events', '[]'::jsonb, 'service_links', '[]'::jsonb),
                1, repeat('a', 64), 'fixture'
             FROM generate_series(1, 100) n",
        )
        .bind(workspace_id.to_string())
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE cloud_project_snapshots SET snapshot = jsonb_set(snapshot, '{events}',
                CASE project_slug
                    WHEN 'p1' THEN '[{\"cloud_key\":\"z\",\"created_at\":\"2026-01-01\"},
                                     {\"cloud_key\":\"a\",\"created_at\":\"2026-01-01\"}]'::jsonb
                    ELSE '[{\"cloud_key\":\"new\",\"created_at\":\"2026-01-02\"}]'::jsonb END)
             WHERE workspace_id = $1::uuid AND project_slug IN ('p1', 'p100')",
        )
        .bind(workspace_id.to_string())
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        assert_eq!(
            store.workspace_context(workspace_id).await.unwrap().len(),
            100
        );
        let events = store.events(workspace_id).await.unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event["cloud_key"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["new", "a", "z"]
        );

        let mut transaction = store.begin_tenant(workspace_id).await.unwrap();
        sqlx::query(
            "INSERT INTO cloud_project_snapshots
                (workspace_id, project_slug, project_name, snapshot, revision, snapshot_hash, pushed_by)
             SELECT workspace_id, 'extra', project_name, snapshot, revision, snapshot_hash, pushed_by
             FROM cloud_project_snapshots WHERE workspace_id = $1::uuid AND project_slug = 'p1'",
        )
        .bind(workspace_id.to_string())
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        for result in [
            store.workspace_context(workspace_id).await,
            store.service_graph(workspace_id).await,
            store.events(workspace_id).await,
        ] {
            assert!(result.unwrap_err().is::<ContextLimitExceeded>());
        }

        let mut transaction = store.begin_tenant(workspace_id).await.unwrap();
        sqlx::query("DELETE FROM cloud_project_snapshots WHERE workspace_id = $1::uuid AND project_slug != 'p1'")
            .bind(workspace_id.to_string())
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE cloud_project_snapshots SET snapshot = jsonb_set(snapshot, '{project,name}',
                to_jsonb(repeat('x', 4194304 - octet_length(snapshot::text) + length(snapshot->'project'->>'name'))))
             WHERE workspace_id = $1::uuid",
        )
        .bind(workspace_id.to_string())
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        assert_eq!(
            store.workspace_context(workspace_id).await.unwrap().len(),
            1
        );
        assert!(store.service_graph(workspace_id).await.unwrap().is_empty());
        assert_eq!(store.events(workspace_id).await.unwrap().len(), 2);

        let mut transaction = store.begin_tenant(workspace_id).await.unwrap();
        sqlx::query(
            "UPDATE cloud_project_snapshots SET snapshot = jsonb_set(snapshot, '{project,name}',
                to_jsonb((snapshot->'project'->>'name') || 'x')) WHERE workspace_id = $1::uuid",
        )
        .bind(workspace_id.to_string())
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        for result in [
            store.workspace_context(workspace_id).await,
            store.service_graph(workspace_id).await,
            store.events(workspace_id).await,
        ] {
            assert!(result.unwrap_err().is::<ContextLimitExceeded>());
        }
    }

    #[tokio::test]
    async fn postgres_replacement_fts_conflict_and_rls() {
        let Ok(database_url) = std::env::var("AI_WORKSPACE_CLOUD_TEST_DATABASE_URL") else {
            return;
        };
        let store = CloudStore::connect(&database_url).await.unwrap();
        let workspace_id = Uuid::new_v4();
        let workspace_slug = format!("store-{}", &workspace_id.simple().to_string()[..12]);
        let project = CloudProject {
            cloud_key: "project:demo".into(),
            name: "Demo".into(),
            slug: "demo".into(),
        };
        let mut snapshot = CloudProjectSnapshot {
            schema_version: CLOUD_SNAPSHOT_SCHEMA_VERSION,
            project: project.clone(),
            groups: vec![],
            shares: vec![],
            documents: vec![],
            notes: vec![CloudNote {
                cloud_key: "note:demo:project:needle".into(),
                project_slug: "demo".into(),
                scope: CloudNoteScope::Project,
                group_slug: None,
                label: Some("searchable".into()),
                content: "unique-cloud-needle".into(),
            }],
            service_links: vec![],
            dependencies: vec![],
            events: vec![],
        };
        let note = &mut snapshot.notes[0];
        let fingerprint = crate::cloud::snapshot::sha256_hex(
            &crate::cloud::models::note_fingerprint_input(
                &note.project_slug,
                note.scope,
                None,
                note.label.as_deref(),
                &note.content,
            )
            .unwrap(),
        );
        note.cloud_key =
            crate::cloud::models::keys::note("demo", note.scope, &fingerprint, 0).unwrap();
        let initial_hash = snapshot_hash(&snapshot);
        let first = store
            .replace_project_snapshot(
                workspace_id,
                &workspace_slug,
                &snapshot,
                &initial_hash,
                None,
                false,
                "fixture-user",
            )
            .await
            .unwrap();
        assert!(matches!(
            first,
            ReplaceSnapshotOutcome::Accepted {
                revision: 1,
                no_op: false,
                ..
            }
        ));
        assert_eq!(
            store
                .replace_project_snapshot(
                    workspace_id,
                    &workspace_slug,
                    &snapshot,
                    &initial_hash,
                    None,
                    false,
                    "retry-user",
                )
                .await
                .unwrap(),
            ReplaceSnapshotOutcome::Accepted {
                revision: 1,
                snapshot_hash: initial_hash.clone(),
                no_op: true,
            }
        );
        assert_eq!(
            store
                .search_documents(workspace_id, "unique-cloud-needle", Some("note"), 20)
                .await
                .unwrap()
                .len(),
            1
        );
        let hidden_without_context: i64 =
            sqlx::query_scalar("SELECT count(*) FROM cloud_project_snapshots")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(hidden_without_context, 0);
        assert!(
            store
                .workspace_context(Uuid::new_v4())
                .await
                .unwrap()
                .is_empty()
        );

        let replacement = CloudProjectSnapshot {
            notes: vec![],
            ..snapshot.clone()
        };
        let replacement_hash = snapshot_hash(&replacement);
        let second = store
            .replace_project_snapshot(
                workspace_id,
                &workspace_slug,
                &replacement,
                &replacement_hash,
                Some(1),
                false,
                "fixture-user",
            )
            .await
            .unwrap();
        assert!(matches!(
            second,
            ReplaceSnapshotOutcome::Accepted { revision: 2, .. }
        ));
        assert!(
            store
                .search_documents(workspace_id, "unique-cloud-needle", Some("note"), 20)
                .await
                .unwrap()
                .is_empty()
        );
        let mut transaction = store.begin_tenant(workspace_id).await.unwrap();
        let before_replay: String = sqlx::query_scalar(
            "SELECT row_to_json(s)::text FROM cloud_project_snapshots s
             WHERE workspace_id = $1::uuid AND project_slug = 'demo'",
        )
        .bind(workspace_id.to_string())
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();

        // Replay A after B, including attempts to choose the server's revision.
        for base_revision in [None, Some(1), Some(-1), Some(i64::MAX)] {
            assert_eq!(
                store
                    .replace_project_snapshot(
                        workspace_id,
                        &workspace_slug,
                        &snapshot,
                        &initial_hash,
                        base_revision,
                        false,
                        "replay-user",
                    )
                    .await
                    .unwrap(),
                ReplaceSnapshotOutcome::Conflict(SnapshotStatus {
                    revision: 2,
                    snapshot_hash: replacement_hash.clone(),
                })
            );
        }
        let mut transaction = store.begin_tenant(workspace_id).await.unwrap();
        let after_replay: String = sqlx::query_scalar(
            "SELECT row_to_json(s)::text FROM cloud_project_snapshots s
             WHERE workspace_id = $1::uuid AND project_slug = 'demo'",
        )
        .bind(workspace_id.to_string())
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        assert_eq!(after_replay, before_replay);
        transaction.commit().await.unwrap();
        assert!(
            store
                .search_documents(workspace_id, "unique-cloud-needle", Some("note"), 20)
                .await
                .unwrap()
                .is_empty()
        );

        assert_eq!(
            store
                .replace_project_snapshot(
                    workspace_id,
                    &workspace_slug,
                    &snapshot,
                    &initial_hash,
                    Some(i64::MAX),
                    true,
                    "force-user",
                )
                .await
                .unwrap(),
            ReplaceSnapshotOutcome::Accepted {
                revision: 3,
                snapshot_hash: initial_hash,
                no_op: false,
            }
        );
        let mut transaction = store.begin_tenant(workspace_id).await.unwrap();
        let audit = sqlx::query(
            "SELECT previous_revision, previous_snapshot_hash, pushed_by, forced
             FROM cloud_project_snapshots
             WHERE workspace_id = $1::uuid AND project_slug = 'demo'",
        )
        .bind(workspace_id.to_string())
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        assert_eq!(audit.get::<Option<i64>, _>("previous_revision"), Some(2));
        assert_eq!(
            audit.get::<Option<String>, _>("previous_snapshot_hash"),
            Some(replacement_hash)
        );
        assert_eq!(audit.get::<String, _>("pushed_by"), "force-user");
        assert!(audit.get::<bool, _>("forced"));
        transaction.commit().await.unwrap();
        assert_eq!(
            store
                .search_documents(workspace_id, "unique-cloud-needle", Some("note"), 20)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn postgres_serializes_concurrent_first_and_revision_pushes() {
        let Ok(database_url) = std::env::var("AI_WORKSPACE_CLOUD_TEST_DATABASE_URL") else {
            return;
        };
        let store = CloudStore::connect(&database_url).await.unwrap();
        let workspace_id = Uuid::new_v4();
        let workspace_slug = format!("race-{}", &workspace_id.simple().to_string()[..12]);
        let snapshot = CloudProjectSnapshot {
            schema_version: CLOUD_SNAPSHOT_SCHEMA_VERSION,
            project: CloudProject {
                cloud_key: "project:race".into(),
                name: "Race".into(),
                slug: "race".into(),
            },
            groups: vec![],
            shares: vec![],
            documents: vec![],
            notes: vec![],
            service_links: vec![],
            dependencies: vec![],
            events: vec![],
        };

        let first_a_hash = snapshot_hash(&snapshot);
        let mut first_b_snapshot = snapshot.clone();
        first_b_snapshot.project.name = "First B".into();
        let first_b_hash = snapshot_hash(&first_b_snapshot);
        let first_a = store.replace_project_snapshot(
            workspace_id,
            &workspace_slug,
            &snapshot,
            &first_a_hash,
            None,
            false,
            "first-a",
        );
        let first_b = store.replace_project_snapshot(
            workspace_id,
            &workspace_slug,
            &first_b_snapshot,
            &first_b_hash,
            None,
            false,
            "first-b",
        );
        let (first_a, first_b) = tokio::join!(first_a, first_b);
        let first = [first_a.unwrap(), first_b.unwrap()];
        assert_eq!(
            first
                .iter()
                .filter(|outcome| matches!(outcome, ReplaceSnapshotOutcome::Accepted { .. }))
                .count(),
            1
        );
        assert_eq!(
            first
                .iter()
                .filter(|outcome| matches!(outcome, ReplaceSnapshotOutcome::Conflict(_)))
                .count(),
            1
        );

        let mut next_a_snapshot = snapshot.clone();
        next_a_snapshot.project.name = "Next A".into();
        let next_a_hash = snapshot_hash(&next_a_snapshot);
        let mut next_b_snapshot = snapshot.clone();
        next_b_snapshot.project.name = "Next B".into();
        let next_b_hash = snapshot_hash(&next_b_snapshot);
        let next_a = store.replace_project_snapshot(
            workspace_id,
            &workspace_slug,
            &next_a_snapshot,
            &next_a_hash,
            Some(1),
            false,
            "next-a",
        );
        let next_b = store.replace_project_snapshot(
            workspace_id,
            &workspace_slug,
            &next_b_snapshot,
            &next_b_hash,
            Some(1),
            false,
            "next-b",
        );
        let (next_a, next_b) = tokio::join!(next_a, next_b);
        let next = [next_a.unwrap(), next_b.unwrap()];
        let winner = next
            .iter()
            .find_map(|outcome| match outcome {
                ReplaceSnapshotOutcome::Accepted {
                    revision: 2,
                    snapshot_hash,
                    no_op: false,
                } => Some(snapshot_hash),
                _ => None,
            })
            .unwrap();
        let conflict = next
            .iter()
            .find_map(|outcome| match outcome {
                ReplaceSnapshotOutcome::Conflict(status) => Some(status),
                _ => None,
            })
            .unwrap();
        assert_eq!(conflict.revision, 2);
        assert_eq!(&conflict.snapshot_hash, winner);
    }
}
