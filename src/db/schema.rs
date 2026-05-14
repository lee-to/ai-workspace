use anyhow::{Context as _, Result};
use log::{debug, error, info};
use rusqlite::{Connection, params};

use crate::models::normalize_project_slug;

const CURRENT_SCHEMA_VERSION: i64 = 3;

pub fn init_db(conn: &Connection) -> Result<()> {
    info!("Initializing database schema");

    conn.execute_batch("PRAGMA journal_mode = WAL;")
        .context("Failed to configure SQLite journal mode")?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .context("Failed to enable SQLite foreign keys")?;
    debug!("WAL mode enabled, foreign keys enforced");

    let detected_version = schema_version(conn)?;
    debug!("Detected database schema version {}", detected_version);

    migrate_schema(conn, detected_version)?;
    run_schema_maintenance(conn)?;

    info!("Database schema initialized");
    Ok(())
}

fn schema_version(conn: &Connection) -> Result<i64> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .context("Failed to read SQLite PRAGMA user_version")
}

fn set_schema_version(conn: &Connection, version: i64) -> Result<()> {
    let sql = format!("PRAGMA user_version = {version}");
    conn.execute_batch(&sql)
        .with_context(|| format!("Failed to set schema version to {version}"))
}

fn migrate_schema(conn: &Connection, from_version: i64) -> Result<()> {
    if from_version > CURRENT_SCHEMA_VERSION {
        error!(
            "Database schema version {} is newer than supported version {}",
            from_version, CURRENT_SCHEMA_VERSION
        );
        anyhow::bail!(
            "Database schema version {} is newer than supported version {}",
            from_version,
            CURRENT_SCHEMA_VERSION
        );
    }

    let mut version = from_version;
    while version < CURRENT_SCHEMA_VERSION {
        let next_version = version + 1;
        debug!(
            "Applying schema migration step {} -> {}",
            version, next_version
        );
        migrate_to_version(conn, next_version)
            .with_context(|| format!("Migration to schema version {next_version} failed"))?;
        set_schema_version(conn, next_version)?;
        version = next_version;
    }

    if from_version < CURRENT_SCHEMA_VERSION {
        info!(
            "Migrated database schema from version {} to {}",
            from_version, CURRENT_SCHEMA_VERSION
        );
    }

    Ok(())
}

fn migrate_to_version(conn: &Connection, version: i64) -> Result<()> {
    match version {
        1 => {
            debug!("Migration v1: create core shared-context schema");
            ensure_core_schema_objects(conn)
        }
        2 => {
            debug!("Migration v2: add project slugs");
            migrate_projects_slug(conn)
        }
        3 => {
            debug!("Migration v3: add service graph and event tables");
            ensure_event_schema_objects(conn)
        }
        _ => {
            error!("No migration registered for schema version {}", version);
            anyhow::bail!("No migration registered for schema version {}", version);
        }
    }
}

fn ensure_core_schema_objects(conn: &Connection) -> Result<()> {
    execute_schema_step(
        conn,
        "core tables, indexes, and triggers",
        "
        CREATE TABLE IF NOT EXISTS projects (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE,
            path TEXT NOT NULL UNIQUE,
            created_at DATETIME NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS groups (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            created_at DATETIME NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS project_groups (
            project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            group_id INTEGER NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
            PRIMARY KEY (project_id, group_id)
        );

        CREATE TABLE IF NOT EXISTS shared_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL CHECK (kind IN ('file', 'dir', 'note')),
            path TEXT,
            content TEXT,
            label TEXT,
            project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE,
            group_id INTEGER REFERENCES groups(id) ON DELETE CASCADE,
            created_by_project_id INTEGER REFERENCES projects(id),
            created_at DATETIME NOT NULL DEFAULT (datetime('now')),
            updated_at DATETIME NOT NULL DEFAULT (datetime('now')),
            CHECK (
                (kind IN ('file', 'dir') AND path IS NOT NULL AND project_id IS NOT NULL AND content IS NULL AND group_id IS NULL)
                OR
                (kind = 'note' AND content IS NOT NULL AND project_id IS NOT NULL AND group_id IS NULL AND path IS NULL)
                OR
                (kind = 'note' AND content IS NOT NULL AND group_id IS NOT NULL AND project_id IS NULL AND path IS NULL AND created_by_project_id IS NOT NULL)
            )
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(label, content);

        CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(
            path,
            content,
            tokenize='unicode61 remove_diacritics 2'
        );

        CREATE TABLE IF NOT EXISTS files_fts_meta (
            shared_item_id INTEGER PRIMARY KEY REFERENCES shared_items(id) ON DELETE CASCADE,
            abs_path TEXT NOT NULL,
            mtime INTEGER NOT NULL,
            size INTEGER NOT NULL,
            indexed_at DATETIME NOT NULL DEFAULT (datetime('now'))
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_shared_items_project_path
        ON shared_items (project_id, path) WHERE path IS NOT NULL;

        CREATE TRIGGER IF NOT EXISTS trg_shared_items_delete_fts
        AFTER DELETE ON shared_items
        BEGIN
            DELETE FROM files_fts WHERE rowid = OLD.id;
        END;

        CREATE TRIGGER IF NOT EXISTS trg_shared_items_delete_notes_fts
        AFTER DELETE ON shared_items
        WHEN OLD.kind = 'note'
        BEGIN
            DELETE FROM notes_fts WHERE rowid = OLD.id;
        END;
        ",
    )?;
    remove_orphaned_notes_fts_rows(conn)?;
    debug!("files_fts virtual table + files_fts_meta created (unicode61 remove_diacritics 2)");
    Ok(())
}

fn remove_orphaned_notes_fts_rows(conn: &Connection) -> Result<()> {
    let removed = conn
        .execute(
            "DELETE FROM notes_fts
             WHERE rowid NOT IN (
                 SELECT id FROM shared_items WHERE kind = 'note'
             )",
            [],
        )
        .context("Failed to remove orphaned notes_fts rows")?;
    if removed > 0 {
        debug!("Removed {} orphaned notes_fts rows", removed);
    }
    Ok(())
}

fn migrate_projects_slug(conn: &Connection) -> Result<()> {
    if project_has_column(conn, "slug")? {
        debug!("projects.slug already exists; continuing migration");
    } else {
        execute_schema_step(
            conn,
            "projects.slug column",
            "ALTER TABLE projects ADD COLUMN slug TEXT;",
        )?;
    }

    backfill_project_slugs(conn)?;

    execute_schema_step(
        conn,
        "projects.slug unique index",
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_slug ON projects(slug);",
    )?;

    Ok(())
}

fn project_has_column(conn: &Connection, column_name: &str) -> Result<bool> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(projects)")
        .context("Failed to inspect projects table columns")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(columns.iter().any(|column| column == column_name))
}

fn backfill_project_slugs(conn: &Connection) -> Result<()> {
    let mut stmt = conn
        .prepare("SELECT id, name, path FROM projects WHERE slug IS NULL OR slug = '' ORDER BY id")
        .context("Failed to prepare projects slug backfill query")?;
    let projects = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Failed to read projects for slug backfill")?;
    drop(stmt);

    let mut used_slugs = existing_project_slugs(conn)?;
    for (id, name, path) in projects {
        let source = if name.trim().is_empty() {
            std::path::Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "project".to_string())
        } else {
            name
        };
        let slug = unique_slug(&normalize_project_slug(&source), &mut used_slugs);
        debug!("Backfilling project id={} with slug={}", id, slug);
        conn.execute(
            "UPDATE projects SET slug = ?1 WHERE id = ?2",
            params![slug, id],
        )
        .with_context(|| format!("Failed to backfill slug for project id={id}"))?;
    }

    Ok(())
}

fn existing_project_slugs(conn: &Connection) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn
        .prepare("SELECT slug FROM projects WHERE slug IS NOT NULL AND slug != ''")
        .context("Failed to prepare existing project slug query")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .collect())
}

fn unique_slug(base: &str, used_slugs: &mut std::collections::HashSet<String>) -> String {
    let mut candidate = base.to_string();
    let mut suffix = 2;
    while used_slugs.contains(&candidate) {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    used_slugs.insert(candidate.clone());
    candidate
}

fn run_schema_maintenance(conn: &Connection) -> Result<()> {
    remove_orphaned_notes_fts_rows(conn)
}

fn execute_schema_step(conn: &Connection, step_name: &str, sql: &str) -> Result<()> {
    debug!("Executing schema step: {}", step_name);
    conn.execute_batch(sql)
        .map_err(|err| {
            error!("Schema step '{}' failed: {}", step_name, err);
            err
        })
        .with_context(|| format!("Failed to initialize schema object: {step_name}"))
}

fn ensure_event_schema_objects(conn: &Connection) -> Result<()> {
    execute_schema_step(
        conn,
        "service graph and event tables",
        "
        CREATE TABLE IF NOT EXISTS service_links (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            from_project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            to_project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            kind TEXT NOT NULL CHECK (kind IN ('depends_on', 'related_to')),
            label TEXT,
            created_at DATETIME NOT NULL DEFAULT (datetime('now')),
            updated_at DATETIME NOT NULL DEFAULT (datetime('now')),
            CHECK (from_project_id != to_project_id),
            UNIQUE (from_project_id, to_project_id, kind)
        );

        CREATE TABLE IF NOT EXISTS artifact_dependencies (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            shared_item_id INTEGER NOT NULL REFERENCES shared_items(id) ON DELETE CASCADE,
            depends_on_project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
            depends_on_project_slug_snapshot TEXT NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('references', 'consumes_api', 'documents', 'configures')),
            reaction TEXT NOT NULL CHECK (reaction IN ('inspect', 'update', 'delete', 'remove_reference')),
            created_at DATETIME NOT NULL DEFAULT (datetime('now')),
            updated_at DATETIME NOT NULL DEFAULT (datetime('now')),
            UNIQUE (shared_item_id, depends_on_project_slug_snapshot, kind)
        );

        CREATE TABLE IF NOT EXISTS workspace_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
            source_project_slug TEXT NOT NULL,
            source_project_name TEXT NOT NULL,
            group_id INTEGER REFERENCES groups(id) ON DELETE SET NULL,
            kind TEXT NOT NULL CHECK (kind IN ('service_deleted', 'service_changed', 'artifact_changed')),
            title TEXT NOT NULL,
            body TEXT,
            severity TEXT NOT NULL CHECK (severity IN ('info', 'warning', 'error', 'critical')) DEFAULT 'info',
            status TEXT NOT NULL CHECK (status IN ('open', 'closed')) DEFAULT 'open',
            created_at DATETIME NOT NULL DEFAULT (datetime('now')),
            updated_at DATETIME NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS event_targets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id INTEGER NOT NULL REFERENCES workspace_events(id) ON DELETE CASCADE,
            affected_project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
            relation_kind TEXT NOT NULL CHECK (relation_kind IN ('linked_service', 'artifact_dependency')),
            status TEXT NOT NULL CHECK (status IN ('open', 'resolved')) DEFAULT 'open',
            created_at DATETIME NOT NULL DEFAULT (datetime('now')),
            updated_at DATETIME NOT NULL DEFAULT (datetime('now')),
            UNIQUE (event_id, affected_project_id, relation_kind)
        );

        CREATE TABLE IF NOT EXISTS event_artifacts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id INTEGER NOT NULL REFERENCES workspace_events(id) ON DELETE CASCADE,
            affected_project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
            shared_item_id INTEGER REFERENCES shared_items(id) ON DELETE SET NULL,
            path_snapshot TEXT NOT NULL,
            reaction TEXT NOT NULL CHECK (reaction IN ('inspect', 'update', 'delete', 'remove_reference')),
            reason TEXT NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('open', 'resolved')) DEFAULT 'open',
            created_at DATETIME NOT NULL DEFAULT (datetime('now')),
            updated_at DATETIME NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_service_links_from ON service_links(from_project_id);
        CREATE INDEX IF NOT EXISTS idx_service_links_to ON service_links(to_project_id);
        CREATE INDEX IF NOT EXISTS idx_artifact_dependencies_item ON artifact_dependencies(shared_item_id);
        CREATE INDEX IF NOT EXISTS idx_artifact_dependencies_project ON artifact_dependencies(depends_on_project_id);
        CREATE INDEX IF NOT EXISTS idx_workspace_events_source ON workspace_events(source_project_id);
        CREATE INDEX IF NOT EXISTS idx_workspace_events_status ON workspace_events(status);
        CREATE INDEX IF NOT EXISTS idx_event_targets_event ON event_targets(event_id);
        CREATE INDEX IF NOT EXISTS idx_event_targets_project ON event_targets(affected_project_id);
        CREATE INDEX IF NOT EXISTS idx_event_artifacts_event ON event_artifacts(event_id);
        CREATE INDEX IF NOT EXISTS idx_event_artifacts_project ON event_artifacts(affected_project_id);
        CREATE INDEX IF NOT EXISTS idx_event_artifacts_item ON event_artifacts(shared_item_id);
        ",
    )?;
    info!("Service graph and event tables initialized");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn init_db_creates_tables() {
        let conn = mem_conn();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(tables.contains(&"projects".to_string()));
        assert!(tables.contains(&"groups".to_string()));
        assert!(tables.contains(&"project_groups".to_string()));
        assert!(tables.contains(&"shared_items".to_string()));
        assert!(tables.contains(&"files_fts_meta".to_string()));
        assert!(tables.contains(&"service_links".to_string()));
        assert!(tables.contains(&"artifact_dependencies".to_string()));
        assert!(tables.contains(&"workspace_events".to_string()));
        assert!(tables.contains(&"event_targets".to_string()));
        assert!(tables.contains(&"event_artifacts".to_string()));
    }

    #[test]
    fn files_fts_virtual_table_created() {
        let conn = mem_conn();
        // Should be able to insert into files_fts without error
        conn.execute(
            "INSERT INTO files_fts (rowid, path, content) VALUES (1, 'a.md', 'hello world')",
            [],
        )
        .unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files_fts WHERE files_fts MATCH 'hello'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn deleting_note_removes_notes_fts_row() {
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO projects (name, slug, path) VALUES ('p', 'p', '/tmp/p')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO shared_items (kind, content, project_id) VALUES ('note', 'hello', 1)",
            [],
        )
        .unwrap();
        let note_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO notes_fts (rowid, label, content) VALUES (?1, 'ctx', 'hello')",
            params![note_id],
        )
        .unwrap();

        conn.execute("DELETE FROM shared_items WHERE id = ?1", params![note_id])
            .unwrap();

        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notes_fts WHERE rowid = ?1",
                params![note_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn init_db_removes_orphaned_notes_fts_rows() {
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO notes_fts (rowid, label, content) VALUES (99, 'orphan', 'stale')",
            [],
        )
        .unwrap();

        init_db(&conn).unwrap();

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM notes_fts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn init_db_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_db(&conn).unwrap(); // second call should not fail
    }

    #[test]
    fn init_db_records_schema_version() {
        let conn = mem_conn();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn init_db_rejects_newer_schema_version() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA user_version = 999").unwrap();
        let result = init_db(&conn);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("newer than supported")
        );
        let created_tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'projects'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            created_tables, 0,
            "future schema versions must be rejected before creating local schema objects"
        );
    }

    #[test]
    fn init_db_migrates_legacy_schema_snapshot() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE projects (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                created_at DATETIME NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE groups (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                created_at DATETIME NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE project_groups (
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                group_id INTEGER NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
                PRIMARY KEY (project_id, group_id)
            );

            CREATE TABLE shared_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL CHECK (kind IN ('file', 'dir', 'note')),
                path TEXT,
                content TEXT,
                label TEXT,
                project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE,
                group_id INTEGER REFERENCES groups(id) ON DELETE CASCADE,
                created_by_project_id INTEGER REFERENCES projects(id),
                created_at DATETIME NOT NULL DEFAULT (datetime('now')),
                updated_at DATETIME NOT NULL DEFAULT (datetime('now')),
                CHECK (
                    (kind IN ('file', 'dir') AND path IS NOT NULL AND project_id IS NOT NULL AND content IS NULL AND group_id IS NULL)
                    OR
                    (kind = 'note' AND content IS NOT NULL AND project_id IS NOT NULL AND group_id IS NULL AND path IS NULL)
                    OR
                    (kind = 'note' AND content IS NOT NULL AND group_id IS NOT NULL AND project_id IS NULL AND path IS NULL AND created_by_project_id IS NOT NULL)
                )
            );

            CREATE VIRTUAL TABLE notes_fts USING fts5(label, content);
            CREATE VIRTUAL TABLE files_fts USING fts5(
                path,
                content,
                tokenize='unicode61 remove_diacritics 2'
            );
            CREATE TABLE files_fts_meta (
                shared_item_id INTEGER PRIMARY KEY REFERENCES shared_items(id) ON DELETE CASCADE,
                abs_path TEXT NOT NULL,
                mtime INTEGER NOT NULL,
                size INTEGER NOT NULL,
                indexed_at DATETIME NOT NULL DEFAULT (datetime('now'))
            );
            CREATE UNIQUE INDEX idx_shared_items_project_path
            ON shared_items (project_id, path) WHERE path IS NOT NULL;

            INSERT INTO projects (id, name, path) VALUES
                (1, 'Legacy API', '/tmp/legacy-api'),
                (2, 'Legacy API', '/tmp/legacy-api-copy');
            INSERT INTO groups (id, name) VALUES (1, 'legacy-group');
            INSERT INTO project_groups (project_id, group_id) VALUES (1, 1);
            INSERT INTO shared_items (id, kind, path, label, project_id)
            VALUES (1, 'file', 'readme.md', 'legacy-readme', 1);
            INSERT INTO shared_items (id, kind, content, label, group_id, created_by_project_id)
            VALUES (2, 'note', 'legacy note content', 'legacy-note', 1, 1);
            INSERT INTO notes_fts (rowid, label, content)
            VALUES (2, 'legacy-note', 'legacy note content');
            INSERT INTO files_fts (rowid, path, content)
            VALUES (1, 'readme.md', 'legacy file token');
            ",
        )
        .unwrap();

        init_db(&conn).unwrap();

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);

        let slugs: Vec<String> = conn
            .prepare("SELECT slug FROM projects ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(slugs, vec!["legacy-api", "legacy-api-2"]);

        let unique_slug_index: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_projects_slug'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unique_slug_index, 1);

        for (kind, name) in [
            ("index", "idx_workspace_events_source"),
            ("trigger", "trg_shared_items_delete_notes_fts"),
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = ?1 AND name = ?2",
                    params![kind, name],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "{name} should exist after migration");
        }

        for table in [
            "service_links",
            "artifact_dependencies",
            "workspace_events",
            "event_targets",
            "event_artifacts",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "{table} should exist after v3 migration");
        }

        let shared_file_label: String = conn
            .query_row(
                "SELECT label FROM shared_items WHERE project_id = 1 AND path = 'readme.md'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(shared_file_label, "legacy-readme");

        let note_matches: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notes_fts WHERE notes_fts MATCH 'legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(note_matches, 1);

        let file_matches: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files_fts WHERE files_fts MATCH 'token'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(file_matches, 1);
    }

    #[test]
    fn foreign_keys_enabled() {
        let conn = mem_conn();
        let fk: i32 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn check_constraint_file_requires_path_and_project() {
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO projects (name, slug, path) VALUES ('p', 'p', '/tmp/p')",
            [],
        )
        .unwrap();
        // file without path should fail
        let result = conn.execute(
            "INSERT INTO shared_items (kind, project_id) VALUES ('file', 1)",
            [],
        );
        assert!(result.is_err());
    }

    #[test]
    fn check_constraint_note_requires_content() {
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO projects (name, slug, path) VALUES ('p', 'p', '/tmp/p')",
            [],
        )
        .unwrap();
        // note without content should fail
        let result = conn.execute(
            "INSERT INTO shared_items (kind, project_id) VALUES ('note', 1)",
            [],
        );
        assert!(result.is_err());
    }

    #[test]
    fn valid_file_insert() {
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO projects (name, slug, path) VALUES ('p', 'p', '/tmp/p')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO shared_items (kind, path, project_id) VALUES ('file', 'test.rs', 1)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn valid_project_note_insert() {
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO projects (name, slug, path) VALUES ('p', 'p', '/tmp/p')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO shared_items (kind, content, project_id) VALUES ('note', 'hello', 1)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn valid_group_note_insert() {
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO projects (name, slug, path) VALUES ('p', 'p', '/tmp/p')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO groups (name) VALUES ('g')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO shared_items (kind, content, group_id, created_by_project_id) VALUES ('note', 'hello', 1, 1)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn unique_index_project_path() {
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO projects (name, slug, path) VALUES ('p', 'p', '/tmp/p')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO shared_items (kind, path, project_id) VALUES ('file', 'a.txt', 1)",
            [],
        )
        .unwrap();
        let result = conn.execute(
            "INSERT INTO shared_items (kind, path, project_id) VALUES ('file', 'a.txt', 1)",
            [],
        );
        assert!(result.is_err());
    }

    #[test]
    fn service_links_reject_invalid_kind() {
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO projects (name, slug, path) VALUES ('a', 'a', '/tmp/a')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO projects (name, slug, path) VALUES ('b', 'b', '/tmp/b')",
            [],
        )
        .unwrap();
        let result = conn.execute(
            "INSERT INTO service_links (from_project_id, to_project_id, kind) VALUES (1, 2, 'invalid')",
            [],
        );
        assert!(result.is_err());
    }

    #[test]
    fn workspace_events_preserve_source_snapshot_after_project_delete() {
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO projects (name, slug, path) VALUES ('p', 'p', '/tmp/p')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workspace_events (source_project_id, source_project_slug, source_project_name, kind, title)
             VALUES (1, 'p', 'p', 'service_deleted', 'deleted')",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM projects WHERE id = 1", [])
            .unwrap();
        let snapshot: String = conn
            .query_row(
                "SELECT source_project_slug FROM workspace_events WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let source_project_id: Option<i64> = conn
            .query_row(
                "SELECT source_project_id FROM workspace_events WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(snapshot, "p");
        assert_eq!(source_project_id, None);
    }
}
