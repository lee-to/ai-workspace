use anyhow::Result;
use log::{debug, info};
use rusqlite::Connection;

pub fn init_db(conn: &Connection) -> Result<()> {
    info!("Initializing database schema");

    conn.execute_batch("PRAGMA journal_mode = WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    debug!("WAL mode enabled, foreign keys enforced");

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS projects (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
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
        ",
    )?;
    debug!("files_fts virtual table + files_fts_meta created (unicode61 remove_diacritics 2)");

    info!("Database schema initialized");
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
    fn init_db_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_db(&conn).unwrap(); // second call should not fail
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
            "INSERT INTO projects (name, path) VALUES ('p', '/tmp/p')",
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
            "INSERT INTO projects (name, path) VALUES ('p', '/tmp/p')",
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
            "INSERT INTO projects (name, path) VALUES ('p', '/tmp/p')",
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
            "INSERT INTO projects (name, path) VALUES ('p', '/tmp/p')",
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
            "INSERT INTO projects (name, path) VALUES ('p', '/tmp/p')",
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
            "INSERT INTO projects (name, path) VALUES ('p', '/tmp/p')",
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
}
