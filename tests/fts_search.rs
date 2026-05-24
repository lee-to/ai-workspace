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

fn run(db: &PathBuf, dir: &std::path::Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(binary_path())
        .args(args)
        .current_dir(dir)
        .env("AI_WORKSPACE_DB", db.to_string_lossy().to_string())
        .env("RUST_LOG", "warn")
        .output()
        .expect("run ai-workspace");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

fn assert_hit_path(stdout: &str, path: &str) {
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with(path) && line.contains("  [id=")),
        "expected hit path {path}: {stdout}"
    );
}

fn raw_fts_hit_count(db: &PathBuf, query: &str) -> i64 {
    let conn = Connection::open(db).unwrap();
    conn.query_row(
        "SELECT COUNT(*)
         FROM files_fts
         WHERE files_fts MATCH ?1",
        params![query],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn cli_init_config_md_share_indexes_before_search() {
    let (_dbdir, db) = temp_db();
    let proj = tempfile::tempdir().unwrap();
    fs::write(proj.path().join("README.md"), "init_config_marker").unwrap();
    fs::write(
        proj.path().join(".ai-workspace.json"),
        r#"{
            "name": "p",
            "share": ["README.md"]
        }"#,
    )
    .unwrap();

    let (stdout, stderr, ok) = run(&db, proj.path(), &["init"]);
    assert!(
        ok,
        "init should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Indexed 1 .md file(s) for search"),
        "init should report immediate indexing\nstdout:\n{stdout}"
    );
    assert_eq!(raw_fts_hit_count(&db, "init_config_marker"), 1);
}

#[test]
fn cli_sync_config_dir_share_indexes_children_before_search() {
    let (_dbdir, db) = temp_db();
    let proj = tempfile::tempdir().unwrap();
    fs::create_dir_all(proj.path().join("docs")).unwrap();
    fs::write(proj.path().join("docs/a.md"), "sync_config_dir_marker").unwrap();
    fs::write(proj.path().join("docs/skip.txt"), "not markdown").unwrap();

    assert!(run(&db, proj.path(), &["init", "--name", "p"]).2);
    fs::write(
        proj.path().join(".ai-workspace.json"),
        r#"{
            "name": "p",
            "share": ["docs"]
        }"#,
    )
    .unwrap();

    let (stdout, stderr, ok) = run(&db, proj.path(), &["sync"]);
    assert!(
        ok,
        "sync should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Indexed 1 .md file(s) for search"),
        "sync should report immediate indexing\nstdout:\n{stdout}"
    );
    assert_eq!(raw_fts_hit_count(&db, "sync_config_dir_marker"), 1);
}

#[test]
fn cli_sync_config_removed_share_removes_raw_fts_rows() {
    let (_dbdir, db) = temp_db();
    let proj = tempfile::tempdir().unwrap();
    fs::write(proj.path().join("old.md"), "removed_config_marker").unwrap();

    assert!(run(&db, proj.path(), &["init", "--name", "p"]).2);
    assert!(run(&db, proj.path(), &["share", "old.md"]).2);
    assert_eq!(raw_fts_hit_count(&db, "removed_config_marker"), 1);

    fs::write(
        proj.path().join(".ai-workspace.json"),
        r#"{
            "name": "p",
            "share": []
        }"#,
    )
    .unwrap();

    let (_stdout, stderr, ok) = run(&db, proj.path(), &["sync"]);
    assert!(ok, "sync should succeed\nstderr:\n{stderr}");
    assert_eq!(raw_fts_hit_count(&db, "removed_config_marker"), 0);
}

#[test]
fn cli_search_finds_indexed_md_file() {
    let (_dbdir, db) = temp_db();
    let proj = tempfile::tempdir().unwrap();
    fs::write(
        proj.path().join("notes.md"),
        "The quick brown fox token_xyz",
    )
    .unwrap();

    assert!(run(&db, proj.path(), &["init", "--name", "p"]).2);
    assert!(run(&db, proj.path(), &["share", "notes.md"]).2);

    let (stdout, _, ok) = run(&db, proj.path(), &["search", "token_xyz"]);
    assert!(ok, "search should succeed");
    assert!(
        stdout.contains("notes.md"),
        "expected hit for notes.md: {}",
        stdout
    );
    assert!(stdout.contains("token_xyz") || stdout.contains("[token_xyz]"));
}

#[test]
fn cli_search_dir_share_indexes_children() {
    let (_dbdir, db) = temp_db();
    let proj = tempfile::tempdir().unwrap();
    fs::create_dir_all(proj.path().join("docs")).unwrap();
    fs::write(
        proj.path().join("docs/a.md"),
        "alpha_topic rust programming",
    )
    .unwrap();
    fs::write(proj.path().join("docs/b.md"), "bravo_topic sqlite fts5").unwrap();
    fs::write(
        proj.path().join("docs/c.txt"),
        "not markdown should be skipped",
    )
    .unwrap();

    run(&db, proj.path(), &["init", "--name", "p"]);
    run(&db, proj.path(), &["share", "docs"]);

    let (stdout, _, _) = run(&db, proj.path(), &["search", "alpha_topic"]);
    assert_hit_path(&stdout, "docs/a.md");

    let (stdout, _, _) = run(&db, proj.path(), &["search", "bravo_topic"]);
    assert_hit_path(&stdout, "docs/b.md");
}

#[test]
fn cli_search_backslash_share_indexes_with_forward_slash_path() {
    let (_dbdir, db) = temp_db();
    let proj = tempfile::tempdir().unwrap();
    fs::create_dir_all(proj.path().join("docs")).unwrap();
    fs::write(
        proj.path().join("docs").join("README.md"),
        "portable_backslash_marker",
    )
    .unwrap();

    run(&db, proj.path(), &["init", "--name", "p"]);
    let (_stdout, stderr, ok) = run(&db, proj.path(), &["share", r"docs\README.md"]);
    assert!(ok, "backslash share should succeed\nstderr:\n{stderr}");

    let (stdout, _, ok) = run(&db, proj.path(), &["search", "portable_backslash_marker"]);
    assert!(ok, "search should succeed");
    assert_hit_path(&stdout, "docs/README.md");
}

#[test]
fn cli_search_dir_share_refreshes_edited_child_file() {
    let (_dbdir, db) = temp_db();
    let proj = tempfile::tempdir().unwrap();
    fs::create_dir_all(proj.path().join("docs")).unwrap();
    fs::write(proj.path().join("docs/a.md"), "old_child_marker").unwrap();
    fs::write(proj.path().join("docs/b.md"), "steady_child_marker").unwrap();

    run(&db, proj.path(), &["init", "--name", "p"]);
    run(&db, proj.path(), &["share", "docs"]);

    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(proj.path().join("docs/a.md"), "new_child_marker").unwrap();

    let (stdout, _, _) = run(&db, proj.path(), &["search", "new_child_marker"]);
    assert_hit_path(&stdout, "docs/a.md");

    let (stdout, _, _) = run(&db, proj.path(), &["search", "old_child_marker"]);
    assert!(
        !stdout.contains("docs/a.md"),
        "old child content should disappear after lazy refresh: {}",
        stdout
    );
}

#[test]
fn cli_search_dir_share_removes_deleted_child_file() {
    let (_dbdir, db) = temp_db();
    let proj = tempfile::tempdir().unwrap();
    fs::create_dir_all(proj.path().join("docs")).unwrap();
    fs::write(proj.path().join("docs/a.md"), "deleted_child_marker").unwrap();
    fs::write(proj.path().join("docs/b.md"), "remaining_child_marker").unwrap();

    run(&db, proj.path(), &["init", "--name", "p"]);
    run(&db, proj.path(), &["share", "docs"]);

    fs::remove_file(proj.path().join("docs/a.md")).unwrap();

    let (stdout, _, _) = run(&db, proj.path(), &["search", "deleted_child_marker"]);
    assert!(
        !stdout.contains("docs/a.md"),
        "deleted child should be removed from the index: {}",
        stdout
    );

    let (stdout, _, _) = run(&db, proj.path(), &["search", "remaining_child_marker"]);
    assert_hit_path(&stdout, "docs/b.md");
}

#[test]
fn cli_search_cyrillic_query() {
    let (_dbdir, db) = temp_db();
    let proj = tempfile::tempdir().unwrap();
    fs::write(
        proj.path().join("ru.md"),
        "Полнотекстовый поиск работает с кириллицей",
    )
    .unwrap();

    run(&db, proj.path(), &["init", "--name", "p"]);
    run(&db, proj.path(), &["share", "ru.md"]);

    let (stdout, _, ok) = run(&db, proj.path(), &["search", "кириллицей"]);
    assert!(ok);
    assert!(
        stdout.contains("ru.md"),
        "cyrillic search failed: {}",
        stdout
    );
}

#[test]
fn cli_search_reflects_file_edits_via_lazy_refresh() {
    let (_dbdir, db) = temp_db();
    let proj = tempfile::tempdir().unwrap();
    fs::write(proj.path().join("t.md"), "initial_marker content here").unwrap();

    run(&db, proj.path(), &["init", "--name", "p"]);
    run(&db, proj.path(), &["share", "t.md"]);

    // Wait >1s so mtime clearly differs
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(proj.path().join("t.md"), "updated_marker fresh content").unwrap();

    let (stdout, _, _) = run(&db, proj.path(), &["search", "updated_marker"]);
    assert!(stdout.contains("t.md"), "refresh failed: {}", stdout);

    let (stdout, _, _) = run(&db, proj.path(), &["search", "initial_marker"]);
    assert!(!stdout.contains("t.md"), "stale content leaked: {}", stdout);
}

#[test]
fn cli_reindex_repopulates_after_wipe() {
    let (_dbdir, db) = temp_db();
    let proj = tempfile::tempdir().unwrap();
    fs::write(proj.path().join("r.md"), "reindexable_marker").unwrap();

    run(&db, proj.path(), &["init", "--name", "p"]);
    run(&db, proj.path(), &["share", "r.md"]);

    let (stdout, _, ok) = run(&db, proj.path(), &["reindex"]);
    assert!(ok);
    assert!(stdout.contains("Reindex complete"));
    let (stdout, _, _) = run(&db, proj.path(), &["search", "reindexable_marker"]);
    assert!(stdout.contains("r.md"));
}

#[test]
fn cli_unshare_removes_from_index() {
    let (_dbdir, db) = temp_db();
    let proj = tempfile::tempdir().unwrap();
    fs::write(proj.path().join("u.md"), "unshare_me_soon_marker").unwrap();

    run(&db, proj.path(), &["init", "--name", "p"]);
    run(&db, proj.path(), &["share", "u.md"]);
    let (stdout, _, _) = run(&db, proj.path(), &["search", "unshare_me_soon_marker"]);
    assert!(stdout.contains("u.md"));

    run(&db, proj.path(), &["rm", "u.md"]);
    let (stdout, _, _) = run(&db, proj.path(), &["search", "unshare_me_soon_marker"]);
    assert!(
        !stdout.contains("u.md"),
        "should be removed from index: {}",
        stdout
    );
}

#[test]
fn cli_search_skips_large_file() {
    let (_dbdir, db) = temp_db();
    let proj = tempfile::tempdir().unwrap();
    let big = "bigfile_marker ".repeat(100_000); // > 1MB
    assert!(big.len() as u64 > 1_024 * 1_024);
    fs::write(proj.path().join("huge.md"), big).unwrap();

    run(&db, proj.path(), &["init", "--name", "p"]);
    run(&db, proj.path(), &["share", "huge.md"]);

    let (stdout, _, _) = run(&db, proj.path(), &["search", "bigfile_marker"]);
    assert!(
        !stdout.contains("huge.md"),
        "large file should have been skipped: {}",
        stdout
    );
}
