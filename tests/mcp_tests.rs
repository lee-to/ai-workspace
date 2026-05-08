use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use rusqlite::{Connection, params};

fn binary_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("target/debug/ai-workspace");
    path
}

fn mcp_request(db_path: &PathBuf, requests: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut child = Command::new(binary_path())
        .arg("serve")
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start MCP server");

    let mut stdin = child.stdin.take().unwrap();
    for req in requests {
        let line = serde_json::to_string(req).unwrap();
        writeln!(stdin, "{}", line).unwrap();
    }
    drop(stdin); // Close stdin to signal EOF

    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("Failed to parse response"))
        .collect()
}

fn temp_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("workspace.db");
    (dir, db_path)
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

/// Seed a project and group via CLI before MCP tests
fn seed_data(db_path: &PathBuf) -> tempfile::TempDir {
    let project_dir = tempfile::tempdir().unwrap();
    std::fs::write(project_dir.path().join("readme.md"), "# Hello").unwrap();

    let run = |args: &[&str]| {
        Command::new(binary_path())
            .args(args)
            .current_dir(project_dir.path())
            .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
            .output()
            .expect("seed command failed");
    };

    run(&["init", "--name", "seed-proj", "--group", "seed-group"]);
    run(&["share", "readme.md", "--label", "readme"]);
    run(&[
        "note",
        "--group",
        "seed-group",
        "--label",
        "deploy-note",
        "This is a test note about deployment",
    ]);

    project_dir
}

#[test]
fn test_mcp_migrates_legacy_database_and_read_paths_work() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    std::fs::write(project_dir.path().join("readme.md"), "legacy file token").unwrap();
    create_legacy_db(&db_path, project_dir.path());

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_context",
                "arguments": {}
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let context: serde_json::Value = serde_json::from_str(content).unwrap();
    assert_eq!(context["projects"][0]["name"], "legacy-proj");
    assert_eq!(context["groups"][0]["name"], "legacy-group");

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "workspace_search",
                "arguments": { "query": "legacy" }
            }
        })],
    );
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("legacy note content"));

    let conn = Connection::open(&db_path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
}

/// Seed a project with a file tree suitable for project_tree/project_grep tests
fn seed_tree_project(db_path: &PathBuf) -> tempfile::TempDir {
    let project_dir = tempfile::tempdir().unwrap();

    // Create file structure
    std::fs::write(
        project_dir.path().join("main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();
    std::fs::create_dir(project_dir.path().join("src")).unwrap();
    std::fs::write(
        project_dir.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.path().join("src/utils.rs"),
        "pub fn greet(name: &str) {\n    println!(\"hello {}\", name);\n}\n",
    )
    .unwrap();

    // Init project via CLI
    Command::new(binary_path())
        .args(["init", "--name", "tree-proj"])
        .current_dir(project_dir.path())
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .output()
        .expect("seed command failed");

    project_dir
}

#[test]
fn test_mcp_initialize() {
    let (_db_dir, db_path) = temp_db();

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        })],
    );

    assert_eq!(responses.len(), 1);
    let result = &responses[0]["result"];
    assert_eq!(result["protocolVersion"], "2024-11-05");
    assert_eq!(result["serverInfo"]["name"], "ai-workspace");
}

#[test]
fn test_mcp_tools_list() {
    let (_db_dir, db_path) = temp_db();

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        })],
    );

    assert_eq!(responses.len(), 1);
    let tools = responses[0]["result"]["tools"].as_array().unwrap();
    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    assert!(tool_names.contains(&"workspace_context"));
    assert!(tool_names.contains(&"workspace_read"));
    assert!(tool_names.contains(&"workspace_search"));
    assert!(tool_names.contains(&"list_groups"));
    assert!(tool_names.contains(&"list_projects"));
}

#[test]
fn test_mcp_workspace_context() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_context",
                "arguments": {}
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let context: serde_json::Value = serde_json::from_str(content).unwrap();
    assert!(context["projects"].as_array().unwrap().len() > 0);
    assert_eq!(context["projects"][0]["name"], "seed-proj");
    assert_eq!(context["projects"][0]["slug"], "seed-proj");
    // Verify labels appear in shared_items
    let shared_items = context["projects"][0]["shared_items"].as_array().unwrap();
    assert!(shared_items.iter().any(|i| i["label"] == "readme"));
}

#[test]
fn test_mcp_workspace_read() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "item_id": 1 }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(content, "# Hello");
}

#[test]
fn test_mcp_workspace_search() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_search",
                "arguments": { "query": "deployment" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let results: Vec<serde_json::Value> = serde_json::from_str(content).unwrap();
    assert!(results.len() > 0);
    assert!(
        results[0]["content"]
            .as_str()
            .unwrap()
            .contains("deployment")
    );
    // Verify label in search results
    assert_eq!(results[0]["label"], "deploy-note");
}

#[test]
fn test_mcp_list_groups() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_groups",
                "arguments": {}
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let groups: Vec<serde_json::Value> = serde_json::from_str(content).unwrap();
    assert_eq!(groups[0]["name"], "seed-group");
    assert!(groups[0]["projects"].as_array().unwrap().len() > 0);
    assert_eq!(groups[0]["projects"][0]["slug"], "seed-proj");
}

#[test]
fn test_mcp_list_projects() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_projects",
                "arguments": {}
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let projects: Vec<serde_json::Value> = serde_json::from_str(content).unwrap();
    assert_eq!(projects[0]["name"], "seed-proj");
    assert_eq!(projects[0]["slug"], "seed-proj");
    assert!(projects[0]["groups"].as_array().unwrap().len() > 0);
}

// --- project_tree tests ---

#[test]
fn test_mcp_project_tree_basic() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_tree",
                "arguments": { "project_id": 1 }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("main.rs"));
    assert!(content.contains("src/"));
    assert!(content.contains("lib.rs"));
    assert!(content.contains("utils.rs"));
}

#[test]
fn test_mcp_project_tree_subpath() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_tree",
                "arguments": { "project_id": 1, "subdir": "src" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("lib.rs"));
    assert!(content.contains("utils.rs"));
    // Should NOT contain main.rs (it's outside src/)
    assert!(!content.contains("main.rs"));
}

#[test]
fn test_mcp_project_tree_invalid_project() {
    let (_db_dir, db_path) = temp_db();

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_tree",
                "arguments": { "project_id": 9999 }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let result = &responses[0]["result"];
    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("not found"));
}

// --- workspace_read by project_id+path tests ---

#[test]
fn test_mcp_workspace_read_by_path() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "project_id": 1, "rel_path": "src/lib.rs" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("pub fn add"));
}

#[test]
fn test_mcp_workspace_read_path_traversal_attack() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "project_id": 1, "rel_path": "../../etc/passwd" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let result = &responses[0]["result"];
    let text = result["content"][0]["text"].as_str().unwrap();
    // Should be denied or fail to resolve
    assert!(
        text.contains("denied") || text.contains("Cannot resolve") || result["isError"] == true
    );
}

#[test]
fn test_mcp_workspace_read_missing_file() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "project_id": 1, "rel_path": "nonexistent.txt" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let result = &responses[0]["result"];
    assert_eq!(result["isError"], true);
}

#[test]
fn test_mcp_workspace_read_backward_compat() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    // item_id still works
    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "item_id": 1 }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(content, "# Hello");
}

#[test]
fn test_mcp_workspace_read_both_params_error() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "item_id": 1, "project_id": 1, "rel_path": "readme.md" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    assert!(responses[0]["error"].is_object());
    assert_eq!(responses[0]["error"]["code"], -32602);
}

// --- project_grep tests ---

#[test]
fn test_mcp_project_grep_basic() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_grep",
                "arguments": { "project_id": 1, "pattern": "println" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("println"));
    // Should match in main.rs and src/utils.rs
    assert!(content.contains("main.rs"));
    assert!(content.contains("utils.rs"));
}

#[test]
fn test_mcp_project_grep_glob_filter() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_grep",
                "arguments": { "project_id": 1, "pattern": "pub fn", "glob": "*.rs" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("pub fn"));
}

#[test]
fn test_mcp_project_grep_invalid_regex() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_grep",
                "arguments": { "project_id": 1, "pattern": "[invalid" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    assert!(responses[0]["error"].is_object());
    assert_eq!(responses[0]["error"]["code"], -32602);
}

#[test]
fn test_mcp_project_grep_no_matches() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_grep",
                "arguments": { "project_id": 1, "pattern": "zzzzz_no_match_zzzzz" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(content, "");
}
