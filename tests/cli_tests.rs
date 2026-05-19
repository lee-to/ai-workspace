use std::fs;
use std::path::PathBuf;
use std::process::Command;

use rusqlite::{Connection, params};

fn binary_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("target/debug/ai-workspace");
    path
}

fn temp_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("workspace.db");
    (dir, db_path)
}

#[cfg(unix)]
fn symlink_path(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

fn assert_shared_label(conn: &Connection, path: &str, label: &str, kind: &str) {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM shared_items
             WHERE project_id = 1
               AND kind = ?1
               AND path = ?2
               AND label = ?3",
            params![kind, path, label],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "{kind} share {path} should have label {label}");
}

fn create_legacy_db(db_path: &PathBuf, project_path: &std::path::Path) {
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;

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
        CREATE TRIGGER trg_shared_items_delete_fts
        AFTER DELETE ON shared_items
        BEGIN
            DELETE FROM files_fts WHERE rowid = OLD.id;
        END;
        ",
    )
    .unwrap();

    let project_path = project_path
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    conn.execute(
        "INSERT INTO projects (id, name, path) VALUES (1, 'legacy-proj', ?1)",
        params![project_path],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO groups (id, name) VALUES (1, 'legacy-group')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_groups (project_id, group_id) VALUES (1, 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO shared_items (id, kind, path, label, project_id) VALUES (1, 'file', 'readme.md', 'legacy-readme', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO shared_items (id, kind, content, label, group_id, created_by_project_id) VALUES (2, 'note', 'legacy note content', 'legacy-note', 1, 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO notes_fts (rowid, label, content) VALUES (2, 'legacy-note', 'legacy note content')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO files_fts (rowid, path, content) VALUES (1, 'readme.md', 'legacy file token')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO files_fts_meta (shared_item_id, abs_path, mtime, size) VALUES (1, '/tmp/legacy/readme.md', 1, 17)",
        [],
    )
    .unwrap();
}

fn run_cmd_in_dir(
    db_path: &PathBuf,
    dir: &std::path::Path,
    args: &[&str],
) -> (String, String, bool) {
    let output = Command::new(binary_path())
        .args(args)
        .current_dir(dir)
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .env("RUST_LOG", "debug")
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, stderr, output.status.success())
}

fn run_cmd_in_dir_with_env(
    db_path: &PathBuf,
    dir: &std::path::Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> (String, String, bool) {
    let mut command = Command::new(binary_path());
    command
        .args(args)
        .current_dir(dir)
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .env("RUST_LOG", "debug");

    for (key, value) in envs {
        command.env(key, value);
    }

    let output = command.output().expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, stderr, output.status.success())
}

fn parse_id(stdout: &str) -> i64 {
    let start = stdout.find("(id=").expect("stdout should contain id") + 4;
    let tail = &stdout[start..];
    let end = tail
        .find(',')
        .or_else(|| tail.find(')'))
        .expect("id should be followed by comma or paren")
        + start;
    stdout[start..end].parse().expect("id should be numeric")
}

#[test]
fn test_legacy_database_migrates_and_cli_read_paths_work() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    fs::write(project_dir.path().join("readme.md"), "legacy file token").unwrap();
    create_legacy_db(&db_path, project_dir.path());

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["list"]);
    assert!(success, "list should migrate legacy db: {stderr}");
    assert!(stdout.contains("legacy-proj"));
    assert!(stdout.contains("legacy-group"));

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(success, "status should read migrated db: {stderr}");
    assert!(stdout.contains("Project: legacy-proj"));
    assert!(stdout.contains("legacy-readme"));
    assert!(stdout.contains("legacy-note"));

    let conn = Connection::open(&db_path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 4);
    let indexed_file_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM indexed_files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(indexed_file_rows, 0);
    let legacy_fts_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM files_fts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(legacy_fts_rows, 0);
    drop(conn);

    let (stdout, stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["search", "legacy"]);
    assert!(
        success,
        "search should lazily repopulate migrated index: {stderr}"
    );
    assert!(
        stdout.contains("readme.md"),
        "migrated search should find the shared file after lazy refresh: {stdout}"
    );

    let conn = Connection::open(&db_path).unwrap();
    let indexed_file_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM indexed_files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(indexed_file_rows, 1);
}

#[test]
fn test_init_creates_project() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "test-project"],
    );
    assert!(success, "init should succeed");
    assert!(stdout.contains("Initialized project 'test-project'"));
    assert!(stdout.contains("slug=test-project"));
}

#[test]
fn test_init_with_explicit_slug() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "Auth Service", "--slug", "auth-api"],
    );
    assert!(success, "init with slug should succeed");
    assert!(stdout.contains("Initialized project 'Auth Service'"));
    assert!(stdout.contains("slug=auth-api"));

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(success, "status should succeed");
    assert!(stdout.contains("Slug: auth-api"));
}

#[test]
fn test_destroy_project_by_slug_outside_project_dir() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();

    let (_stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "Auth Service", "--slug", "auth"],
    );
    assert!(success, "init should succeed");

    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, other_dir.path(), &["destroy", "auth"]);
    assert!(success, "destroy slug should work outside project dir");
    assert!(stdout.contains("Removed project 'Auth Service'"));
}

#[test]
fn test_init_with_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "my-proj", "--group", "my-group"],
    );
    assert!(success, "init with group should succeed");
    assert!(stdout.contains("Initialized project 'my-proj'"));
    assert!(stdout.contains("Joined group 'my-group'"));
}

#[test]
fn test_init_idempotent() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    assert!(success, "second init should succeed");
    assert!(stdout.contains("already initialized"));
}

#[test]
fn test_init_ai_factory_preset_creates_and_shares_baseline() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--preset", "ai-factory"],
    );
    assert!(
        success,
        "init --preset ai-factory should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("Applied ai-factory preset: files +3, shares +3"));

    for path in [
        ".ai-factory/DESCRIPTION.md",
        ".ai-factory/ARCHITECTURE.md",
        ".ai-factory/PLAN.md",
    ] {
        assert!(
            project_dir.path().join(path).exists(),
            "{path} should exist"
        );
    }

    let conn = Connection::open(&db_path).unwrap();
    assert_shared_label(
        &conn,
        ".ai-factory/DESCRIPTION.md",
        "ai-factory-description",
        "file",
    );
    assert_shared_label(
        &conn,
        ".ai-factory/ARCHITECTURE.md",
        "ai-factory-architecture",
        "file",
    );
    assert_shared_label(&conn, ".ai-factory/PLAN.md", "ai-factory-plan", "file");
}

#[test]
fn test_init_ai_factory_preset_is_idempotent_and_preserves_files() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    fs::create_dir(project_dir.path().join(".ai-factory")).unwrap();
    fs::write(
        project_dir.path().join(".ai-factory/DESCRIPTION.md"),
        "custom description",
    )
    .unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--preset", "ai-factory"],
    );
    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--preset", "ai-factory"],
    );
    assert!(
        success,
        "second init --preset ai-factory should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("Applied ai-factory preset: files +0, shares +0"));
    assert_eq!(
        fs::read_to_string(project_dir.path().join(".ai-factory/DESCRIPTION.md")).unwrap(),
        "custom description"
    );

    let conn = Connection::open(&db_path).unwrap();
    let shared_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM shared_items
             WHERE project_id = 1
               AND path IN (
                 '.ai-factory/DESCRIPTION.md',
                 '.ai-factory/ARCHITECTURE.md',
                 '.ai-factory/PLAN.md'
               )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(shared_count, 3);
}

#[test]
fn test_init_ai_factory_preset_shares_optional_dirs_when_present() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    fs::create_dir(project_dir.path().join(".ai-factory")).unwrap();
    fs::create_dir(project_dir.path().join(".ai-factory/references")).unwrap();
    fs::create_dir(project_dir.path().join(".ai-factory/patches")).unwrap();
    fs::create_dir(project_dir.path().join(".ai-factory/specs")).unwrap();

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--preset", "ai-factory"],
    );
    assert!(
        success,
        "init --preset ai-factory should share optional dirs\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("Applied ai-factory preset: files +3, shares +6"));

    let conn = Connection::open(&db_path).unwrap();
    assert_shared_label(
        &conn,
        ".ai-factory/references",
        "ai-factory-references",
        "dir",
    );
    assert_shared_label(&conn, ".ai-factory/patches", "ai-factory-patches", "dir");
    assert_shared_label(&conn, ".ai-factory/specs", "ai-factory-specs", "dir");
}

#[cfg(unix)]
#[test]
fn test_init_ai_factory_preset_rejects_symlinked_ai_factory_dir() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    let outside_dir = tempfile::tempdir().unwrap();
    symlink_path(outside_dir.path(), &project_dir.path().join(".ai-factory")).unwrap();

    let (_stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--preset", "ai-factory"],
    );

    assert!(!success, "symlinked .ai-factory should fail");
    assert!(
        stderr.contains("ai-factory preset parent is not a directory")
            || stderr.contains("ai-factory preset path escapes project root"),
        "stderr should explain preset boundary failure: {stderr}"
    );
    assert!(
        !outside_dir.path().join("DESCRIPTION.md").exists(),
        "preset must not write through .ai-factory symlink"
    );
}

#[cfg(unix)]
#[test]
fn test_init_ai_factory_preset_rejects_symlinked_preset_file() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    fs::create_dir(project_dir.path().join(".ai-factory")).unwrap();
    symlink_path(
        std::path::Path::new("../outside-description.md"),
        &project_dir.path().join(".ai-factory/DESCRIPTION.md"),
    )
    .unwrap();

    let (_stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--preset", "ai-factory"],
    );

    assert!(!success, "symlinked preset file should fail");
    assert!(
        stderr.contains("ai-factory preset file target is not a regular file"),
        "stderr should explain regular-file requirement: {stderr}"
    );
}

#[test]
fn test_init_ai_factory_preset_rejects_directory_at_preset_file_path() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    fs::create_dir(project_dir.path().join(".ai-factory")).unwrap();
    fs::create_dir(project_dir.path().join(".ai-factory/DESCRIPTION.md")).unwrap();

    let (_stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--preset", "ai-factory"],
    );

    assert!(!success, "directory at preset file path should fail");
    assert!(
        stderr.contains("ai-factory preset file target is not a regular file"),
        "stderr should explain regular-file requirement: {stderr}"
    );
}

#[test]
fn test_init_ai_factory_preset_relabels_optional_dirs_on_rerun() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    fs::create_dir(project_dir.path().join(".ai-factory")).unwrap();
    fs::create_dir(project_dir.path().join(".ai-factory/references")).unwrap();
    fs::create_dir(project_dir.path().join(".ai-factory/patches")).unwrap();
    fs::create_dir(project_dir.path().join(".ai-factory/specs")).unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--preset", "ai-factory"],
    );

    let conn = Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE shared_items SET label = 'old-references'
         WHERE path = '.ai-factory/references'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE shared_items SET label = 'old-patches'
         WHERE path = '.ai-factory/patches'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE shared_items SET label = 'old-specs'
         WHERE path = '.ai-factory/specs'",
        [],
    )
    .unwrap();

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--preset", "ai-factory"],
    );

    assert!(
        success,
        "rerun should relabel optional dirs\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("Applied ai-factory preset: files +0, shares +0, labels ~3"));
    assert_shared_label(
        &conn,
        ".ai-factory/references",
        "ai-factory-references",
        "dir",
    );
    assert_shared_label(&conn, ".ai-factory/patches", "ai-factory-patches", "dir");
    assert_shared_label(&conn, ".ai-factory/specs", "ai-factory-specs", "dir");
}

#[test]
fn test_init_ai_factory_preset_updates_existing_workspace_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    fs::write(project_dir.path().join("readme.md"), "# Existing").unwrap();
    fs::write(
        project_dir.path().join(".ai-workspace.json"),
        r#"{
  "name": "proj",
  "share": [
    { "path": "readme.md", "label": "readme", "kind": "file" }
  ]
}
"#,
    )
    .unwrap();

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--preset", "ai-factory"],
    );

    assert!(
        success,
        "init should update existing config\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let config: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project_dir.path().join(".ai-workspace.json")).unwrap(),
    )
    .unwrap();
    let shares = config["share"].as_array().unwrap();
    assert!(shares.iter().any(|share| {
        share["path"] == "readme.md" && share["label"] == "readme" && share["kind"] == "file"
    }));
    assert!(shares.iter().any(|share| {
        share["path"] == ".ai-factory/DESCRIPTION.md"
            && share["label"] == "ai-factory-description"
            && share["kind"] == "file"
    }));
    assert!(shares.iter().any(|share| {
        share["path"] == ".ai-factory/ARCHITECTURE.md"
            && share["label"] == "ai-factory-architecture"
            && share["kind"] == "file"
    }));
    assert!(shares.iter().any(|share| {
        share["path"] == ".ai-factory/PLAN.md"
            && share["label"] == "ai-factory-plan"
            && share["kind"] == "file"
    }));
}

#[test]
fn test_share_file() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("hello.txt"), "hello world").unwrap();
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["share", "hello.txt", "--label", "greeting"],
    );
    assert!(success, "share should succeed");
    assert!(stdout.contains("Shared 'hello.txt'"));
}

#[test]
fn test_share_directory() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::create_dir(project_dir.path().join("docs")).unwrap();
    fs::write(project_dir.path().join("docs/README.md"), "# Docs").unwrap();
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["share", "docs", "--label", "documentation"],
    );
    assert!(success, "share dir should succeed");
    assert!(stdout.contains("Shared dir 'docs'"));
}

#[test]
fn test_share_nonexistent_file() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    let (_stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["share", "nonexistent.txt"]);
    assert!(!success, "sharing nonexistent file should fail");
}

#[test]
fn test_note_group_scope() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &[
            "note",
            "--group",
            "g1",
            "--label",
            "deploy-rule",
            "Always deploy on Fridays",
        ],
    );
    assert!(success, "group note should succeed");
    assert!(stdout.contains("Added note"));
    assert!(stdout.contains("to group 'g1'"));
}

#[test]
fn test_note_group_scope_rejects_non_member_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        other_dir.path(),
        &["init", "--name", "other", "--group", "team"],
    );
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    let (_stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["note", "--group", "team", "outside group"],
    );

    assert!(
        !success,
        "group note in non-member group should fail\nstderr:\n{stderr}"
    );
    assert!(stderr.contains(
        "Project is not a member of group 'team'. Run `ai-workspace init --group team` first."
    ));
}

#[test]
fn test_note_project_scope() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &[
            "note",
            "--scope",
            "project",
            "--label",
            "api-note",
            "This project exposes REST API",
        ],
    );
    assert!(success, "project note should succeed");
    assert!(stdout.contains("Added project note"));
}

#[test]
fn test_rm_by_label() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("config.toml"), "[settings]").unwrap();
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["share", "config.toml", "--label", "config"],
    );

    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["rm", "config"]);
    assert!(success, "rm by label should succeed");
    assert!(stdout.contains("Removed item with label 'config'"));
}

#[test]
fn test_rm_ambiguous_label_requires_id() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("first.txt"), "first").unwrap();
    fs::write(project_dir.path().join("second.txt"), "second").unwrap();
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    let (first_stdout, _, _) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["share", "first.txt", "--label", "dup"],
    );
    let (second_stdout, _, _) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["share", "second.txt", "--label", "dup"],
    );
    let first_id = parse_id(&first_stdout);
    let second_id = parse_id(&second_stdout);

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["rm", "dup"]);

    assert!(!success, "ambiguous rm should fail");
    assert!(stdout.contains("ID"));
    assert!(stdout.contains("Kind"));
    assert!(stdout.contains("Label"));
    assert!(stdout.contains("Value"));
    assert!(stdout.contains("Scope"));
    assert!(stdout.contains("Source"));
    assert!(stdout.contains(&first_id.to_string()));
    assert!(stdout.contains(&second_id.to_string()));
    assert!(stderr.contains("Label 'dup' matches multiple items. Re-run with item ID."));

    let (stdout, _, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["rm", &first_id.to_string()]);
    assert!(success, "rm by explicit id should succeed");
    assert!(stdout.contains(&format!("Removed item id={first_id}")));

    let (stdout, _, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(success);
    assert!(!stdout.contains("first.txt"));
    assert!(stdout.contains("second.txt"));
}

#[test]
fn test_rm_by_path() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("data.json"), "{}").unwrap();
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    run_cmd_in_dir(&db_path, project_dir.path(), &["share", "data.json"]);

    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["rm", "data.json"]);
    assert!(success, "rm by path should succeed");
    assert!(stdout.contains("Removed item with path 'data.json'"));
}

#[test]
fn test_status() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(success, "status should succeed");
    assert!(stdout.contains("Project: proj"));
    assert!(stdout.contains("g1"));
}

#[test]
fn test_sync() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("temp.txt"), "temp").unwrap();
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    run_cmd_in_dir(&db_path, project_dir.path(), &["share", "temp.txt"]);

    fs::remove_file(project_dir.path().join("temp.txt")).unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["sync"]);
    assert!(success, "sync should succeed");
    assert!(stdout.contains("Removed 1 stale entries"));
}

// --- List ---

#[test]
fn test_list_empty() {
    let (_db_dir, db_path) = temp_db();
    let dir = tempfile::tempdir().unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, dir.path(), &["list"]);
    assert!(success, "list on empty workspace should succeed");
    assert!(stdout.contains("Projects: (none)"));
    assert!(stdout.contains("Groups: (none)"));
}

#[test]
fn test_list_projects_and_groups() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj-a", "--group", "team-x"],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["list"]);
    assert!(success, "list should succeed");
    assert!(stdout.contains("proj-a"));
    assert!(stdout.contains("team-x"));
}

#[test]
fn test_list_projects_only() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj-b", "--group", "grp"],
    );

    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["list", "projects"]);
    assert!(success, "list projects should succeed");
    assert!(stdout.contains("proj-b"));
    assert!(!stdout.contains("Groups:"));
}

#[test]
fn test_list_groups_only() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj-c", "--group", "grp-z"],
    );

    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["list", "groups"]);
    assert!(success, "list groups should succeed");
    assert!(stdout.contains("grp-z"));
    assert!(!stdout.contains("Projects:"));
}

#[test]
fn test_list_no_project_required() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();

    // Init project in one dir
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "remote-proj"],
    );

    // List from a different dir (not a project)
    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, other_dir.path(), &["list"]);
    assert!(success, "list should work outside project dir");
    assert!(stdout.contains("remote-proj"));
}

#[test]
fn test_link_add_list_and_rm_by_slug() {
    let (_db_dir, db_path) = temp_db();
    let auth_dir = tempfile::tempdir().unwrap();
    let billing_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        auth_dir.path(),
        &[
            "init",
            "--name",
            "Auth Service",
            "--slug",
            "auth",
            "--group",
            "platform",
        ],
    );
    run_cmd_in_dir(
        &db_path,
        billing_dir.path(),
        &[
            "init",
            "--name",
            "Billing API",
            "--slug",
            "billing",
            "--group",
            "platform",
        ],
    );

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        billing_dir.path(),
        &[
            "link",
            "add",
            "billing",
            "auth",
            "--kind",
            "depends_on",
            "--label",
            "JWT",
        ],
    );
    assert!(success, "link add should succeed: {stderr}");
    assert!(stdout.contains("Linked billing -> auth"));
    let link_id = parse_id(&stdout);

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, billing_dir.path(), &["link", "list"]);
    assert!(success, "link list should succeed: {stderr}");
    assert!(stdout.contains("billing"));
    assert!(stdout.contains("auth"));
    assert!(stdout.contains("depends_on"));
    assert!(stdout.contains("JWT"));

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        billing_dir.path(),
        &["link", "list", "--project", "auth"],
    );
    assert!(success, "link list --project should succeed: {stderr}");
    assert!(stdout.contains("Incoming links for auth"));
    assert!(stdout.contains("billing"));

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        billing_dir.path(),
        &["link", "rm", &link_id.to_string()],
    );
    assert!(success, "link rm should succeed: {stderr}");
    assert!(stdout.contains("Removed link billing -> auth"));

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, billing_dir.path(), &["link", "list"]);
    assert!(success, "link list after rm should succeed: {stderr}");
    assert!(stdout.contains("Service links: (none)"));
}

#[test]
fn test_link_rejects_invalid_kind_with_clap_values() {
    let (_db_dir, db_path) = temp_db();
    let auth_dir = tempfile::tempdir().unwrap();
    let billing_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        auth_dir.path(),
        &["init", "--name", "Auth Service", "--slug", "auth"],
    );
    run_cmd_in_dir(
        &db_path,
        billing_dir.path(),
        &["init", "--name", "Billing API", "--slug", "billing"],
    );

    let (_stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        billing_dir.path(),
        &["link", "add", "billing", "auth", "--kind", "invalid"],
    );
    assert!(!success, "invalid link kind should fail");
    assert!(stderr.contains("depends_on"));
    assert!(stderr.contains("related_to"));
}

#[test]
fn test_artifact_dependency_commands_and_status_summary() {
    let (_db_dir, db_path) = temp_db();
    let auth_dir = tempfile::tempdir().unwrap();
    let api_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        auth_dir.path(),
        &[
            "init",
            "--name",
            "Auth Service",
            "--slug",
            "auth",
            "--group",
            "platform",
        ],
    );
    fs::write(api_dir.path().join("auth.md"), "# Auth contract").unwrap();
    run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &[
            "init",
            "--name",
            "API Service",
            "--slug",
            "api",
            "--group",
            "platform",
        ],
    );
    run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["share", "auth.md", "--label", "auth-contract"],
    );

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &[
            "artifact",
            "depends",
            "auth-contract",
            "auth",
            "--kind",
            "references",
            "--reaction",
            "update",
        ],
    );
    assert!(success, "artifact depends should succeed: {stderr}");
    assert!(stdout.contains("depending on 'auth'"));

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, api_dir.path(), &["artifact", "deps"]);
    assert!(success, "artifact deps should succeed: {stderr}");
    assert!(stdout.contains("auth.md"));
    assert!(stdout.contains("auth"));
    assert!(stdout.contains("references"));
    assert!(stdout.contains("update"));

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, api_dir.path(), &["status"]);
    assert!(success, "status should succeed: {stderr}");
    assert!(stdout.contains("Dependencies"));
    assert!(stdout.contains("auth:references:update"));

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &[
            "artifact",
            "undepend",
            "auth-contract",
            "auth",
            "--kind",
            "references",
        ],
    );
    assert!(success, "artifact undepend should succeed: {stderr}");
    assert!(stdout.contains("Removed 1 artifact dependency"));

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, api_dir.path(), &["artifact", "deps"]);
    assert!(
        success,
        "artifact deps after undepend should succeed: {stderr}"
    );
    assert!(stdout.contains("Artifact dependencies: (none)"));
}

#[test]
fn test_artifact_depends_ambiguous_item_label_requires_id() {
    let (_db_dir, db_path) = temp_db();
    let api_dir = tempfile::tempdir().unwrap();
    let auth_dir = tempfile::tempdir().unwrap();

    fs::write(api_dir.path().join("first.yaml"), "first").unwrap();
    fs::write(api_dir.path().join("second.yaml"), "second").unwrap();
    run_cmd_in_dir(
        &db_path,
        auth_dir.path(),
        &[
            "init", "--name", "Auth", "--slug", "auth", "--group", "core",
        ],
    );
    run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["init", "--name", "API", "--slug", "api", "--group", "core"],
    );
    let (first_stdout, _, _) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["share", "first.yaml", "--label", "contract"],
    );
    let (second_stdout, _, _) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["share", "second.yaml", "--label", "contract"],
    );
    let first_id = parse_id(&first_stdout);
    let second_id = parse_id(&second_stdout);

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &[
            "artifact",
            "depends",
            "contract",
            "auth",
            "--kind",
            "references",
            "--reaction",
            "update",
        ],
    );

    assert!(!success, "ambiguous artifact target should fail");
    assert!(stdout.contains(&first_id.to_string()));
    assert!(stdout.contains(&second_id.to_string()));
    assert!(stderr.contains("Label 'contract' matches multiple items. Re-run with item ID."));

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &[
            "artifact",
            "depends",
            &first_id.to_string(),
            "auth",
            "--kind",
            "references",
            "--reaction",
            "update",
        ],
    );
    assert!(success, "artifact depends by id should succeed: {stderr}");
    assert!(stdout.contains("Marked"));
}

#[test]
fn test_artifact_deps_ambiguous_item_label_requires_id() {
    let (_db_dir, db_path) = temp_db();
    let api_dir = tempfile::tempdir().unwrap();
    let auth_dir = tempfile::tempdir().unwrap();

    fs::write(api_dir.path().join("first.yaml"), "first").unwrap();
    fs::write(api_dir.path().join("second.yaml"), "second").unwrap();
    run_cmd_in_dir(
        &db_path,
        auth_dir.path(),
        &[
            "init", "--name", "Auth", "--slug", "auth", "--group", "core",
        ],
    );
    run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["init", "--name", "API", "--slug", "api", "--group", "core"],
    );
    let (first_stdout, _, _) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["share", "first.yaml", "--label", "contract"],
    );
    let (second_stdout, _, _) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["share", "second.yaml", "--label", "contract"],
    );
    let first_id = parse_id(&first_stdout);
    let second_id = parse_id(&second_stdout);

    let (_stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &[
            "artifact",
            "depends",
            &first_id.to_string(),
            "auth",
            "--kind",
            "references",
            "--reaction",
            "update",
        ],
    );
    assert!(success, "artifact depends by id should succeed: {stderr}");

    let (stdout, stderr, success) =
        run_cmd_in_dir(&db_path, api_dir.path(), &["artifact", "deps", "contract"]);

    assert!(!success, "ambiguous artifact deps target should fail");
    assert!(stdout.contains(&first_id.to_string()));
    assert!(stdout.contains(&second_id.to_string()));
    assert!(stderr.contains("Label 'contract' matches multiple items. Re-run with item ID."));

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["artifact", "deps", &first_id.to_string()],
    );
    assert!(success, "artifact deps by id should succeed: {stderr}");
    assert!(stdout.contains("first.yaml"));
    assert!(stdout.contains("auth"));
    assert!(stdout.contains("references"));
    assert!(stdout.contains("update"));
}

#[test]
fn test_artifact_undepend_ambiguous_item_label_requires_id() {
    let (_db_dir, db_path) = temp_db();
    let api_dir = tempfile::tempdir().unwrap();
    let auth_dir = tempfile::tempdir().unwrap();

    fs::write(api_dir.path().join("first.yaml"), "first").unwrap();
    fs::write(api_dir.path().join("second.yaml"), "second").unwrap();
    run_cmd_in_dir(
        &db_path,
        auth_dir.path(),
        &[
            "init", "--name", "Auth", "--slug", "auth", "--group", "core",
        ],
    );
    run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["init", "--name", "API", "--slug", "api", "--group", "core"],
    );
    let (first_stdout, _, _) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["share", "first.yaml", "--label", "contract"],
    );
    let (second_stdout, _, _) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["share", "second.yaml", "--label", "contract"],
    );
    let first_id = parse_id(&first_stdout);
    let second_id = parse_id(&second_stdout);

    let (_stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &[
            "artifact",
            "depends",
            &first_id.to_string(),
            "auth",
            "--kind",
            "references",
            "--reaction",
            "update",
        ],
    );
    assert!(success, "artifact depends by id should succeed: {stderr}");

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &[
            "artifact",
            "undepend",
            "contract",
            "auth",
            "--kind",
            "references",
        ],
    );

    assert!(!success, "ambiguous artifact undepend target should fail");
    assert!(stdout.contains(&first_id.to_string()));
    assert!(stdout.contains(&second_id.to_string()));
    assert!(stderr.contains("Label 'contract' matches multiple items. Re-run with item ID."));

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &[
            "artifact",
            "undepend",
            &first_id.to_string(),
            "auth",
            "--kind",
            "references",
        ],
    );
    assert!(success, "artifact undepend by id should succeed: {stderr}");
    assert!(stdout.contains("Removed 1 artifact dependency"));
}

#[test]
fn test_artifact_dependency_rejects_invalid_reaction_with_clap_values() {
    let (_db_dir, db_path) = temp_db();
    let auth_dir = tempfile::tempdir().unwrap();
    let api_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        auth_dir.path(),
        &["init", "--name", "Auth Service", "--slug", "auth"],
    );
    fs::write(api_dir.path().join("auth.md"), "# Auth contract").unwrap();
    run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["init", "--name", "API Service", "--slug", "api"],
    );
    run_cmd_in_dir(&db_path, api_dir.path(), &["share", "auth.md"]);

    let (_stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &[
            "artifact",
            "depends",
            "auth.md",
            "auth",
            "--kind",
            "references",
            "--reaction",
            "invalid",
        ],
    );
    assert!(!success, "invalid reaction should fail");
    assert!(stderr.contains("inspect"));
    assert!(stderr.contains("remove_reference"));
}

#[test]
fn test_event_commands_create_inbox_close_and_rm() {
    let (_db_dir, db_path) = temp_db();
    let auth_dir = tempfile::tempdir().unwrap();
    let api_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        auth_dir.path(),
        &[
            "init",
            "--name",
            "Auth Service",
            "--slug",
            "auth",
            "--group",
            "platform",
        ],
    );
    run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &[
            "init",
            "--name",
            "API Service",
            "--slug",
            "api",
            "--group",
            "platform",
        ],
    );
    run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["link", "add", "api", "auth", "--kind", "depends_on"],
    );

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        auth_dir.path(),
        &[
            "event",
            "create",
            "--kind",
            "service_changed",
            "--source",
            "auth",
            "--severity",
            "warning",
            "--title",
            "Auth changed",
            "--body",
            "Token format changed",
        ],
    );
    assert!(success, "event create should succeed: {stderr}");
    assert!(stdout.contains("Created event 'Auth changed'"));
    assert!(stdout.contains("Affected services"));
    assert!(stdout.contains("api"));
    let event_id = parse_id(&stdout);

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, api_dir.path(), &["event", "inbox"]);
    assert!(success, "event inbox should succeed: {stderr}");
    assert!(stdout.contains("Auth changed"));
    assert!(stdout.contains("service_changed"));

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["event", "show", &event_id.to_string()],
    );
    assert!(success, "event show should succeed: {stderr}");
    assert!(stdout.contains("Token format changed"));
    assert!(stdout.contains("linked_service"));

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["event", "close", &event_id.to_string()],
    );
    assert!(success, "event close should succeed: {stderr}");
    assert!(stdout.contains("Closed event"));

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["event", "list", "--source", "auth", "--status", "closed"],
    );
    assert!(success, "event list should succeed: {stderr}");
    assert!(stdout.contains("closed"));
    assert!(stdout.contains("Auth changed"));

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["event", "rm", &event_id.to_string()],
    );
    assert!(success, "event rm should succeed: {stderr}");
    assert!(stdout.contains("Removed event"));
}

#[test]
fn test_destroy_generates_service_deleted_event_with_artifacts() {
    let (_db_dir, db_path) = temp_db();
    let auth_dir = tempfile::tempdir().unwrap();
    let api_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        auth_dir.path(),
        &[
            "init",
            "--name",
            "Auth Service",
            "--slug",
            "auth",
            "--group",
            "platform",
        ],
    );
    fs::write(api_dir.path().join("auth.md"), "# Auth contract").unwrap();
    run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &[
            "init",
            "--name",
            "API Service",
            "--slug",
            "api",
            "--group",
            "platform",
        ],
    );
    run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["share", "auth.md", "--label", "auth-contract"],
    );
    run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["link", "add", "api", "auth", "--kind", "depends_on"],
    );
    run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &[
            "artifact",
            "depends",
            "auth-contract",
            "auth",
            "--kind",
            "references",
            "--reaction",
            "update",
        ],
    );

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, api_dir.path(), &["destroy", "auth"]);
    assert!(success, "destroy should succeed: {stderr}");
    assert!(stdout.contains("Removed project 'Auth Service'"));
    assert!(stdout.contains("event id="));

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        api_dir.path(),
        &["event", "list", "--source", "auth"],
    );
    assert!(success, "event list after destroy should succeed: {stderr}");
    assert!(stdout.contains("service_deleted"));
    assert!(stdout.contains("Service deleted: Auth Service"));

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, api_dir.path(), &["event", "inbox"]);
    assert!(
        success,
        "event inbox after destroy should succeed: {stderr}"
    );
    assert!(stdout.contains("service_deleted"));
    assert!(stdout.contains("Service deleted: Auth Service"));
}

#[test]
fn test_destroy_orphaned_project_by_registered_path() {
    let (_db_dir, db_path) = temp_db();
    let root_dir = tempfile::tempdir().unwrap();
    let old_path = root_dir.path().join("project-old");
    let new_path = root_dir.path().join("project-new");
    let other_dir = tempfile::tempdir().unwrap();

    fs::create_dir(&old_path).unwrap();
    run_cmd_in_dir(&db_path, &old_path, &["init", "--name", "old-proj"]);
    let registered_old_path = old_path.canonicalize().unwrap();

    fs::rename(&old_path, &new_path).unwrap();
    run_cmd_in_dir(&db_path, &new_path, &["init", "--name", "new-proj"]);

    let old_path_string = registered_old_path.to_string_lossy().to_string();
    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        other_dir.path(),
        &["destroy", "--target", &old_path_string],
    );
    assert!(
        success,
        "destroy --target path should remove orphaned project"
    );
    assert!(stdout.contains("Removed project 'old-proj'"));

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, other_dir.path(), &["list"]);
    assert!(success, "list should succeed");
    assert!(!stdout.contains("old-proj"));
    assert!(stdout.contains("new-proj"));
}

#[test]
fn test_destroy_project_by_id_outside_project_dir() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();

    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "doomed"]);
    assert!(success, "init should succeed");
    let id = parse_id(&stdout).to_string();

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, other_dir.path(), &["destroy", &id]);
    assert!(success, "destroy id should work outside project dir");
    assert!(stdout.contains("Removed project 'doomed'"));

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, other_dir.path(), &["list"]);
    assert!(success, "list should succeed");
    assert!(!stdout.contains("doomed"));
}

#[test]
fn test_destroy_project_with_group_note() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["note", "--group", "g1", "--label", "ctx", "group note"],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["destroy"]);
    assert!(success, "destroy should remove project-created group notes");
    assert!(stdout.contains("Removed project 'proj'"));
}

// --- Edit ---

#[test]
fn test_edit_note_content() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["note", "--scope", "project", "--label", "info", "old text"],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["edit", "info", "--content", "new text"],
    );
    assert!(success, "edit content should succeed");
    assert!(stdout.contains("Updated item"));

    // Verify via status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("new text"));
}

#[test]
fn test_edit_note_label() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &[
            "note",
            "--scope",
            "project",
            "--label",
            "old-label",
            "content",
        ],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["edit", "old-label", "--label", "new-label"],
    );
    assert!(success, "edit label should succeed");
    assert!(stdout.contains("Updated item"));

    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("new-label"));
}

#[test]
fn test_edit_ambiguous_label_requires_id() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("notes.md"), "file").unwrap();
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    let (file_stdout, _, _) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["share", "notes.md", "--label", "dup"],
    );
    let (note_stdout, _, _) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["note", "--scope", "project", "--label", "dup", "old text"],
    );
    let file_id = parse_id(&file_stdout);
    let note_id = parse_id(&note_stdout);

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["edit", "dup", "--label", "renamed"],
    );

    assert!(!success, "ambiguous edit should fail");
    assert!(stdout.contains(&file_id.to_string()));
    assert!(stdout.contains(&note_id.to_string()));
    assert!(stdout.contains("notes.md"));
    assert!(stdout.contains("old text"));
    assert!(stderr.contains("Label 'dup' matches multiple items. Re-run with item ID."));

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["edit", &note_id.to_string(), "--label", "renamed"],
    );
    assert!(success, "edit by explicit id should succeed: {stderr}");
    assert!(stdout.contains("Updated item"));

    let (stdout, _, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(success);
    assert!(stdout.contains("renamed"));
    assert!(stdout.contains("notes.md"));
}

#[test]
fn test_edit_scope_project_to_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &[
            "note",
            "--scope",
            "project",
            "--label",
            "my-note",
            "scope test",
        ],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["edit", "my-note", "--scope", "group", "--group", "g1"],
    );
    assert!(success, "edit scope to group should succeed");
    assert!(stdout.contains("Updated item"));

    // Verify: note should now appear in group notes section of status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("Group 'g1' shared items:"));
    assert!(stdout.contains("scope test"));
}

#[test]
fn test_edit_scope_project_to_non_member_group_fails() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        other_dir.path(),
        &["init", "--name", "other", "--group", "team"],
    );
    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &[
            "note",
            "--scope",
            "project",
            "--label",
            "my-note",
            "scope test",
        ],
    );

    let (_stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["edit", "my-note", "--scope", "group", "--group", "team"],
    );

    assert!(
        !success,
        "moving note to non-member group should fail\nstderr:\n{stderr}"
    );
    assert!(stderr.contains(
        "Project is not a member of group 'team'. Run `ai-workspace init --group team` first."
    ));

    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("scope test"));
    assert!(!stdout.contains("Group 'team' shared items:"));
}

#[test]
fn test_edit_scope_group_to_project() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &[
            "note",
            "--scope",
            "group",
            "--group",
            "g1",
            "--label",
            "grp-note",
            "group text",
        ],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["edit", "grp-note", "--scope", "project"],
    );
    assert!(success, "edit scope to project should succeed");
    assert!(stdout.contains("Updated item"));

    // Verify: note should appear in shared items, not group notes
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("group text"));
    assert!(!stdout.contains("Group notes in 'g1'"));
}

#[test]
fn test_edit_no_flags_fails() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["note", "--scope", "project", "--label", "lbl", "text"],
    );

    let (_stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["edit", "lbl"]);
    assert!(!success, "edit with no flags should fail");
}

#[test]
fn test_edit_nonexistent_fails() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    let (_stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["edit", "ghost", "--label", "x"],
    );
    assert!(!success, "edit nonexistent should fail");
}

// --- Leave & Delete Group ---

#[test]
fn test_leave_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["leave", "g1"]);
    assert!(success, "leave should succeed");
    assert!(stdout.contains("Left group 'g1'"));
    assert!(
        stdout.contains("was deleted"),
        "group with no members should be auto-deleted"
    );

    // Verify group is gone from status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("(none)"));
}

#[test]
fn test_leave_group_not_member() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );

    // Leave first time — group gets auto-deleted (no members left)
    run_cmd_in_dir(&db_path, project_dir.path(), &["leave", "g1"]);

    // Leave again — group no longer exists
    let (_stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["leave", "g1"]);
    assert!(!success, "leave should fail because group was auto-deleted");
}

#[test]
fn test_leave_nonexistent_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    let (_stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["leave", "nope"]);
    assert!(!success, "leave nonexistent group should fail");
}

#[test]
fn test_delete_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "g1"],
    );
    // Add a group note
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["note", "--group", "g1", "group note"],
    );

    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["delete-group", "g1"]);
    assert!(success, "delete-group should succeed");
    assert!(stdout.contains("Deleted group 'g1'"));

    // Verify group is gone from status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("Groups: (none)"));
    assert!(!stdout.contains("Group 'g1' shared items:"));
}

#[test]
fn test_delete_nonexistent_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);

    let (_stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["delete-group", "nope"]);
    assert!(!success, "delete nonexistent group should fail");
}

// --- Export & Config ---

#[test]
fn test_export_creates_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("readme.md"), "# Hello").unwrap();
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "my-proj", "--group", "team"],
    );
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["share", "readme.md", "--label", "docs"],
    );
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &[
            "note",
            "--scope",
            "project",
            "--label",
            "info",
            "important note",
        ],
    );

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["export"]);
    assert!(success, "export should succeed");
    assert!(stdout.contains("Exported config"));

    let config_path = project_dir.path().join(".ai-workspace.json");
    assert!(config_path.exists(), ".ai-workspace.json should be created");

    let content = fs::read_to_string(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(config["name"], "my-proj");
    assert_eq!(config["slug"], "my-proj");
    assert_eq!(config["groups"][0], "team");
    assert_eq!(config["share"][0]["path"], "readme.md");
    assert_eq!(config["share"][0]["label"], "docs");
    assert_eq!(config["share"][0]["kind"], "file");
    assert!(config["share"][0]["dependencies"].is_null());
    assert_eq!(config["notes"][0]["content"], "important note");
}

#[test]
fn test_export_config_flag_writes_custom_path_and_updates_it() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("a.txt"), "a").unwrap();
    fs::write(project_dir.path().join("b.txt"), "b").unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    run_cmd_in_dir(&db_path, project_dir.path(), &["share", "a.txt"]);

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["--config", ".ai/ai-workspace.json", "export"],
    );
    assert!(
        success,
        "export should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let custom_config = project_dir.path().join(".ai/ai-workspace.json");
    assert!(
        custom_config.exists(),
        "export should create the custom config path"
    );
    assert!(
        !project_dir.path().join(".ai-workspace.json").exists(),
        "export with --config should not create root .ai-workspace.json"
    );

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["--config", ".ai/ai-workspace.json", "share", "b.txt"],
    );

    let content = fs::read_to_string(custom_config).unwrap();
    assert!(content.contains("a.txt"));
    assert!(content.contains("b.txt"));
}

#[test]
fn test_init_uses_custom_config_from_env() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::create_dir(project_dir.path().join(".ai")).unwrap();
    fs::write(project_dir.path().join("Cargo.toml"), "[package]").unwrap();
    fs::write(project_dir.path().join("README.md"), "# Test").unwrap();
    fs::write(
        project_dir.path().join(".ai/ai-workspace.json"),
        r#"{"name": "from-env", "groups": ["team"], "share": [], "notes": []}"#,
    )
    .unwrap();

    let (stdout, stderr, success) = run_cmd_in_dir_with_env(
        &db_path,
        project_dir.path(),
        &["init"],
        &[("AI_WORKSPACE_CONFIG", ".ai/ai-workspace.json")],
    );
    assert!(
        success,
        "init should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("Auto-shared"),
        "custom config should disable auto-share\nstdout:\n{stdout}"
    );

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(success, "status should succeed\nstderr:\n{stderr}");
    assert!(stdout.contains("from-env"));
    assert!(stdout.contains("team"));
    assert!(!stdout.contains("Cargo.toml"));
    assert!(!stdout.contains("README.md"));
}

#[test]
fn test_init_reads_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    // Write .ai-workspace.json before init
    let config = r#"{
        "name": "from-json",
        "slug": "from-json-slug",
        "groups": ["team-a"],
        "share": ["README.md"],
        "notes": [{"content": "hello from json", "label": "greeting"}]
    }"#;
    fs::write(project_dir.path().join(".ai-workspace.json"), config).unwrap();
    fs::write(project_dir.path().join("README.md"), "# Readme").unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["init"]);
    assert!(success, "init with json should succeed");
    assert!(stdout.contains("from-json"));
    assert!(stdout.contains("slug=from-json-slug"));

    // Verify state via status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("from-json"));
    assert!(stdout.contains("from-json-slug"));
    assert!(stdout.contains("team-a"));
    assert!(stdout.contains("greeting"));
}

#[test]
fn test_init_rejects_json_share_path_traversal() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    let config = r#"{
        "name": "unsafe",
        "share": ["../outside.md"]
    }"#;
    fs::write(project_dir.path().join(".ai-workspace.json"), config).unwrap();

    let (_stdout, stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--group", "team-a"]);
    assert!(
        !success,
        "init should reject path traversal from config\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("outside project directory"),
        "stderr should explain unsafe path\nstderr:\n{stderr}"
    );

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["list"]);
    assert!(success, "list should succeed after rejected init: {stderr}");
    assert!(
        !stdout.contains("unsafe") && !stdout.contains("team-a"),
        "rejected init should not persist project or group state\nstdout:\n{stdout}"
    );
}

#[cfg(unix)]
#[test]
fn test_init_rejects_json_share_symlink_escape() {
    use std::os::unix::fs::symlink;

    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    symlink(outside.path(), project_dir.path().join("escape.md")).unwrap();

    let config = r#"{
        "name": "unsafe",
        "share": ["escape.md"]
    }"#;
    fs::write(project_dir.path().join(".ai-workspace.json"), config).unwrap();

    let (_stdout, stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["init"]);
    assert!(
        !success,
        "init should reject symlink escape from config\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("outside project directory"),
        "stderr should explain unsafe symlink\nstderr:\n{stderr}"
    );
}

#[test]
fn test_init_syncs_config_share_kind_and_dependencies() {
    let (_db_dir, db_path) = temp_db();
    let auth_dir = tempfile::tempdir().unwrap();
    let api_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        auth_dir.path(),
        &["init", "--name", "Auth", "--slug", "auth"],
    );
    fs::create_dir(api_dir.path().join("docs")).unwrap();
    fs::write(api_dir.path().join("docs/auth.md"), "# Auth").unwrap();

    let config = r#"{
        "name": "API",
        "slug": "api",
        "share": [
            {
                "path": "docs",
                "kind": "dir",
                "dependencies": [
                    {
                        "service": "auth",
                        "kind": "references",
                        "reaction": "update"
                    }
                ]
            }
        ]
    }"#;
    fs::write(api_dir.path().join(".ai-workspace.json"), config).unwrap();

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, api_dir.path(), &["init"]);
    assert!(
        success,
        "init should sync config dependencies\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("dependencies +1 -0 ~0"));

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, api_dir.path(), &["status"]);
    assert!(success, "status should succeed");
    assert!(stdout.contains("docs"));
    assert!(stdout.contains("dir"));
    assert!(stdout.contains("auth:references:update"));
}

#[test]
fn test_init_group_updates_existing_json_without_removing_group() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("Makefile"), "all:").unwrap();
    fs::write(project_dir.path().join("README.md"), "# Readme").unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "obs"]);
    run_cmd_in_dir(&db_path, project_dir.path(), &["export"]);

    let config_path = project_dir.path().join(".ai-workspace.json");
    let before = fs::read_to_string(&config_path).unwrap();
    assert!(
        !before.contains("\"groups\""),
        "empty groups should be omitted before the regression step: {before}"
    );

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--group", "integra"],
    );
    assert!(
        success,
        "init --group should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("Joined group 'integra'"));
    assert!(
        !stdout.contains("groups +0 -1"),
        "init --group must not remove the CLI group\nstdout:\n{stdout}"
    );

    let (status, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(
        status.contains("integra"),
        "status should retain the joined group\nstatus:\n{status}"
    );

    let after = fs::read_to_string(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_str(&after).unwrap();
    let groups = config["groups"].as_array().unwrap();
    assert_eq!(
        groups.as_slice(),
        &[serde_json::Value::String("integra".to_string())],
        "config should persist the CLI group after init --group: {after}"
    );

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--group", "integra"],
    );
    assert!(
        success,
        "re-running init --group should stay idempotent\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("groups +0 -1"),
        "idempotent init --group must not remove the group\nstdout:\n{stdout}"
    );

    let after_second = fs::read_to_string(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_str(&after_second).unwrap();
    let groups = config["groups"].as_array().unwrap();
    assert_eq!(
        groups.as_slice(),
        &[serde_json::Value::String("integra".to_string())],
        "config should not duplicate the CLI group: {after_second}"
    );
}

#[test]
fn test_init_group_is_additive_to_existing_json_groups() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    let config = r#"{
        "name": "from-json",
        "groups": ["team-a"],
        "share": ["README.md"]
    }"#;
    fs::write(project_dir.path().join(".ai-workspace.json"), config).unwrap();
    fs::write(project_dir.path().join("README.md"), "# Readme").unwrap();

    let (stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--group", "integra"],
    );
    assert!(
        success,
        "init --group with existing JSON groups should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("Joined group 'integra'"));
    assert!(
        !stdout.contains("groups +0 -1"),
        "init --group must not remove any JSON groups\nstdout:\n{stdout}"
    );

    let (status, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(
        status.contains("team-a"),
        "status should retain the group from JSON\nstatus:\n{status}"
    );
    assert!(
        status.contains("integra"),
        "status should include the CLI group\nstatus:\n{status}"
    );

    let after = fs::read_to_string(project_dir.path().join(".ai-workspace.json")).unwrap();
    let config: serde_json::Value = serde_json::from_str(&after).unwrap();
    let groups = config["groups"].as_array().unwrap();
    assert_eq!(
        groups.as_slice(),
        &[
            serde_json::Value::String("team-a".to_string()),
            serde_json::Value::String("integra".to_string()),
        ],
        "config should contain both JSON and CLI groups: {after}"
    );
}

#[test]
fn test_init_name_flag_overrides_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    let config = r#"{"name": "json-name", "groups": []}"#;
    fs::write(project_dir.path().join(".ai-workspace.json"), config).unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "cli-name"],
    );
    assert!(success);
    assert!(stdout.contains("cli-name"));

    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("cli-name"));
}

#[test]
fn test_share_updates_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("a.txt"), "a").unwrap();
    fs::write(project_dir.path().join("b.txt"), "b").unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    run_cmd_in_dir(&db_path, project_dir.path(), &["share", "a.txt"]);

    // Export to create .json
    run_cmd_in_dir(&db_path, project_dir.path(), &["export"]);

    // Share another file — should update .json
    run_cmd_in_dir(&db_path, project_dir.path(), &["share", "b.txt"]);

    let content = fs::read_to_string(project_dir.path().join(".ai-workspace.json")).unwrap();
    assert!(
        content.contains("b.txt"),
        ".json should be updated with new share"
    );
}

#[test]
fn test_group_note_does_not_update_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "proj", "--group", "grp"],
    );

    // Export to create .json
    run_cmd_in_dir(&db_path, project_dir.path(), &["export"]);
    let before = fs::read_to_string(project_dir.path().join(".ai-workspace.json")).unwrap();

    // Add group note — should NOT update .json
    run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["note", "--scope", "group", "group secret"],
    );

    let after = fs::read_to_string(project_dir.path().join(".ai-workspace.json")).unwrap();
    assert_eq!(before, after, "group note should not modify .json");
    assert!(!after.contains("group secret"));
}

#[test]
fn test_solo_dev_no_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("file.txt"), "x").unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "solo"]);
    run_cmd_in_dir(&db_path, project_dir.path(), &["share", "file.txt"]);

    // No .ai-workspace.json should exist
    assert!(
        !project_dir.path().join(".ai-workspace.json").exists(),
        "solo dev without export should not have .json"
    );
}

#[test]
fn test_sync_with_json() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    fs::write(project_dir.path().join("new-file.txt"), "new").unwrap();

    // Write a config with shares and notes
    let config = r#"{
        "name": "proj",
        "groups": ["team"],
        "share": ["new-file.txt"],
        "notes": [{"content": "synced note", "label": "sync-lbl"}]
    }"#;
    fs::write(project_dir.path().join(".ai-workspace.json"), config).unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["sync"]);
    assert!(success, "sync with json should succeed");
    assert!(stdout.contains("Config sync:") || stdout.contains("Config is in sync"));

    // Verify via status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("team"));
    assert!(stdout.contains("sync-lbl"));
}

// --- Auto-share on init ---

#[test]
fn test_auto_share_rust_project() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(
        project_dir.path().join("Cargo.toml"),
        "[package]\nname = \"test\"",
    )
    .unwrap();
    fs::write(project_dir.path().join("README.md"), "# Test").unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "rust-proj"],
    );
    assert!(success, "init should succeed");
    assert!(stdout.contains("Auto-shared 2 key file(s)"));

    // Verify via status
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("Cargo.toml"));
    assert!(stdout.contains("README.md"));
}

#[test]
fn test_auto_share_node_project() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(
        project_dir.path().join("package.json"),
        "{\"name\": \"test\"}",
    )
    .unwrap();
    fs::write(project_dir.path().join("README.rst"), "Test").unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "node-proj"],
    );
    assert!(success, "init should succeed");
    assert!(stdout.contains("Auto-shared 2 key file(s)"));

    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(stdout.contains("package.json"));
    assert!(stdout.contains("README.rst"));
}

#[test]
fn test_auto_share_skipped_when_json_exists() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("Cargo.toml"), "[package]").unwrap();
    fs::write(project_dir.path().join("README.md"), "# Test").unwrap();
    fs::write(
        project_dir.path().join(".ai-workspace.json"),
        "{\"name\": \"proj\", \"groups\": [], \"share\": [], \"notes\": []}",
    )
    .unwrap();

    let (stdout, _stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["init"]);
    assert!(success, "init should succeed");
    // Should NOT auto-share when .ai-workspace.json exists
    assert!(!stdout.contains("Auto-shared"));

    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    assert!(!stdout.contains("Cargo.toml"));
}

#[test]
fn test_auto_share_no_duplicates_on_reinit() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    fs::write(project_dir.path().join("Cargo.toml"), "[package]").unwrap();
    fs::write(project_dir.path().join("README.md"), "# Test").unwrap();

    // First init — auto-shares
    let (stdout, _stderr, _) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    assert!(stdout.contains("Auto-shared 2 key file(s)"));

    // Second init — should not auto-share again (already shared)
    let (stdout, _stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["init", "--name", "proj"]);
    assert!(success, "re-init should succeed");
    assert!(!stdout.contains("Auto-shared"));

    // Verify only 2 shared items, not 4
    let (stdout, _, _) = run_cmd_in_dir(&db_path, project_dir.path(), &["status"]);
    let cargo_count = stdout.matches("Cargo.toml").count();
    assert_eq!(cargo_count, 1, "Cargo.toml should appear exactly once");
}
