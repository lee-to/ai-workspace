use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusqlite::Connection;

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

fn run_cmd_in_dir(db_path: &Path, dir: &Path, args: &[&str]) -> (String, String, bool) {
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

fn mcp_request(db_path: &Path, requests: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut command = Command::new(binary_path());
    command
        .arg("serve")
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .env_remove("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().expect("Failed to start MCP server");
    let mut stdin = child.stdin.take().unwrap();
    for req in requests {
        writeln!(stdin, "{}", serde_json::to_string(req).unwrap()).unwrap();
    }
    drop(stdin);

    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("Failed to parse MCP response"))
        .collect()
}

#[cfg(unix)]
fn symlink_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn symlink_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(src, dst)
}

fn shared_file_dir_rows(db_path: &Path) -> Vec<(String, String, String)> {
    let conn = Connection::open(db_path).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT path, label, kind
             FROM shared_items
             WHERE kind IN ('file', 'dir')
             ORDER BY path, label, kind",
        )
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn share_paths(config: &serde_json::Value) -> Vec<String> {
    config["share"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap().to_string())
        .collect()
}

fn assert_empty_table_if_exists(conn: &Connection, table: &str) {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .unwrap();
    if exists == 0 {
        return;
    }

    let count: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0, "{table} should not contain persisted rows");
}

fn assert_no_persisted_projects_or_shares(db_path: &Path) {
    if !db_path.exists() {
        return;
    }

    let conn = Connection::open(db_path).unwrap();
    assert_empty_table_if_exists(&conn, "projects");
    assert_empty_table_if_exists(&conn, "shared_items");
}

#[test]
fn non_persistence_assertion_accepts_missing_or_uninitialized_database() {
    let (_db_dir, db_path) = temp_db();

    assert_no_persisted_projects_or_shares(&db_path);
    drop(Connection::open(&db_path).unwrap());
    assert_no_persisted_projects_or_shares(&db_path);
}

#[test]
fn cli_config_backslash_import_export_and_sync_roundtrip_are_stable() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    std::fs::create_dir(project_dir.path().join("docs")).unwrap();
    std::fs::create_dir(project_dir.path().join("examples")).unwrap();
    std::fs::write(project_dir.path().join("docs").join("README.md"), "# Docs").unwrap();

    let config = r#"{
        "name": "portable-roundtrip",
        "share": [
            {"path": "docs\\README.md", "label": "readme", "kind": "file"},
            {"path": "examples\\", "label": "examples", "kind": "dir"},
            {"path": "examples/", "label": "duplicate-examples", "kind": "dir"}
        ]
    }"#;
    std::fs::write(project_dir.path().join(".ai-workspace.json"), config).unwrap();

    let (stdout, stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["init"]);
    assert!(
        success,
        "init should import portable config paths\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    assert_eq!(
        shared_file_dir_rows(&db_path),
        vec![
            (
                "docs/README.md".to_string(),
                "readme".to_string(),
                "file".to_string()
            ),
            (
                "examples".to_string(),
                "examples".to_string(),
                "dir".to_string()
            ),
        ]
    );

    let (_stdout, stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["export"]);
    assert!(success, "export should succeed\nstderr:\n{stderr}");

    let config_path = project_dir.path().join(".ai-workspace.json");
    let exported: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    let paths = share_paths(&exported);
    assert_eq!(paths, vec!["docs/README.md", "examples"]);
    assert!(
        paths
            .iter()
            .all(|path| !path.contains('\\') && !path.ends_with('/')),
        "exported paths should use stable slash form: {paths:?}"
    );

    let (_stdout, stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["sync"]);
    assert!(success, "sync should succeed\nstderr:\n{stderr}");

    let (_stdout, stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["export"]);
    assert!(success, "second export should succeed\nstderr:\n{stderr}");
    let exported_after_sync: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(exported_after_sync, exported);
}

#[test]
fn config_import_rejects_symlink_escape_without_persisting_state() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    let link_path = project_dir.path().join("escape.md");

    if let Err(err) = symlink_file(outside.path(), &link_path) {
        eprintln!(
            "skipping symlink escape regression test because symlink creation failed: {}",
            err
        );
        return;
    }

    let config = r#"{
        "name": "unsafe",
        "share": ["escape.md"]
    }"#;
    std::fs::write(project_dir.path().join(".ai-workspace.json"), config).unwrap();

    let (_stdout, stderr, success) = run_cmd_in_dir(&db_path, project_dir.path(), &["init"]);
    assert!(
        !success,
        "init should reject symlink escape from config\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("outside project directory"),
        "stderr should explain unsafe symlink\nstderr:\n{stderr}"
    );

    assert_no_persisted_projects_or_shares(&db_path);
}

#[test]
fn mcp_workspace_read_accepts_slash_and_backslash_paths_for_shared_dir_file() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();

    std::fs::create_dir(project_dir.path().join("docs")).unwrap();
    std::fs::write(
        project_dir.path().join("docs").join("guide.md"),
        "shared_docs_token\n",
    )
    .unwrap();

    let (_stdout, stderr, success) = run_cmd_in_dir(
        &db_path,
        project_dir.path(),
        &["init", "--name", "mcp-paths"],
    );
    assert!(success, "init should succeed\nstderr:\n{stderr}");
    let (_stdout, stderr, success) =
        run_cmd_in_dir(&db_path, project_dir.path(), &["share", "docs"]);
    assert!(success, "share should succeed\nstderr:\n{stderr}");

    let responses = mcp_request(
        &db_path,
        &[
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "workspace_read",
                    "arguments": { "project_id": 1, "rel_path": "docs/guide.md" }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "workspace_read",
                    "arguments": { "project_id": 1, "rel_path": "docs\\guide.md" }
                }
            }),
        ],
    );

    assert_eq!(responses.len(), 2);
    let slash_content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let backslash_content = responses[1]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(slash_content, "shared_docs_token\n");
    assert_eq!(backslash_content, slash_content);
}
